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
    InverterTempHigh,
    InverterTempLow,
    BatterySocHigh,
    BatterySocLow,
    InverterTrip,
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
    /// The poll loop has lost contact with the inverter (connection dropped
    /// and reconnection is in progress). This is a connection-state event
    /// rather than a snapshot-derived alert, so it is fired by the poll
    /// loop directly rather than by [`evaluate_alerts`].
    ConnectionLost,
}

impl AlertType {
    /// Human-readable name for the alert.
    pub fn human_name(&self) -> &'static str {
        match self {
            Self::BatteryTempHigh => "Battery Temperature High",
            Self::BatteryTempLow => "Battery Temperature Low",
            Self::InverterTempHigh => "Inverter Temperature High",
            Self::InverterTempLow => "Inverter Temperature Low",
            Self::BatterySocHigh => "Battery SOC High",
            Self::BatterySocLow => "Battery SOC Low",
            Self::InverterTrip => "Inverter Trip",
            Self::GridOffline => "Grid Offline",
            // Deliberately distinct from "Battery Temperature High": this one
            // is the inverter's own hardware warning flag (IR 57), not the
            // user's °C threshold being breached. Renamed from
            // "Battery Over-Temperature" to avoid users conflating it with
            // the threshold-based alert.
            Self::BatteryOverTemp => "Inverter Battery Warning",
            Self::SolarClipping => "Solar Clipping",
            Self::ConnectionLost => "Inverter Connection Lost",
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
    let has_pushover =
        !config.pushover_app_token.is_empty() && !config.pushover_user_key.is_empty();

    if !has_telegram && !has_ntfy && !has_pushover {
        return Vec::new();
    }

    let mut alerts = Vec::new();

    // Battery temperature
    let temp = snapshot.battery_temperature;
    if temp.is_finite() {
        if config.batt_temp_max > 0.0 && temp > config.batt_temp_max {
            alerts.push(AlertType::BatteryTempHigh);
        }
        if config.batt_temp_min > 0.0 && temp < config.batt_temp_min {
            alerts.push(AlertType::BatteryTempLow);
        }
    }

    // Inverter temperature
    let inverter_temp = snapshot.inverter_temperature;
    if inverter_temp.is_finite() {
        if config.inverter_temp_max > 0.0 && inverter_temp > config.inverter_temp_max {
            alerts.push(AlertType::InverterTempHigh);
        }
        if config.inverter_temp_min > 0.0 && inverter_temp < config.inverter_temp_min {
            alerts.push(AlertType::InverterTempLow);
        }
    }

    // Battery SOC
    let soc = snapshot.soc;
    if config.soc_min > 0 && soc < config.soc_min {
        alerts.push(AlertType::BatterySocLow);
    }
    if config.soc_max < 100 && soc > config.soc_max {
        alerts.push(AlertType::BatterySocHigh);
    }
    // Grid offline: only grid-presence faults. Inverter trips and battery
    // warnings are separate alert types with their own toggles.
    if config.grid_offline_enabled && (snapshot.grid_loss || !snapshot.grid_online) {
        alerts.push(AlertType::GridOffline);
    }

    // Inverter trip/fault state.
    if config.inverter_trip_enabled && snapshot.inverter_trip {
        alerts.push(AlertType::InverterTrip);
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

/// Build a "lost contact" notification message. Fired by the poll loop
/// (not by [`evaluate_alerts`], which has no access to connection state).
pub fn build_connection_lost_message(host: &str) -> String {
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut msg = format!("📡 HEM Connection Lost — {}\n", time);
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str(&format!(
        "Lost contact with inverter at <b>{host}</b>.\nReconnection is in progress."
    ));
    msg
}

/// Build a "contact restored" notification message.
pub fn build_connection_restored_message(host: &str) -> String {
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut msg = format!("✅ HEM Connection Restored — {}\n", time);
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str(&format!("Contact with <b>{host}</b> re-established."));
    msg
}

/// Send a connection-lost notification to all configured channels. Called
/// from the poll loop's disconnect path. Honours the `connection_lost_enabled`
/// toggle and the standard channel-config gates.
pub async fn send_connection_lost_notification(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    host: &str,
) {
    let config = state.alert_config.lock().await.clone();
    if !config.enabled || !config.connection_lost_enabled {
        return;
    }
    let text = build_connection_lost_message(host);
    let token = config.telegram_bot_token.clone();
    let chat_id = config.telegram_chat_id.clone();
    let ntfy_topic = config.ntfy_topic.clone();
    let ntfy_server = config.ntfy_server.clone();
    let pushover_token = config.pushover_app_token.clone();
    let pushover_key = config.pushover_user_key.clone();
    let text_clone = text.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if !token.is_empty() && !chat_id.is_empty() {
            if let Err(e) = send_telegram_message(&token, &chat_id, &text_clone) {
                tracing::warn!("Telegram connection-lost notification failed: {e}");
            }
        }
        if !ntfy_topic.is_empty() {
            if let Err(e) = send_ntfy_message(&ntfy_topic, &ntfy_server, &text_clone) {
                tracing::warn!("ntfy connection-lost notification failed: {e}");
            }
        }
        if !pushover_token.is_empty() && !pushover_key.is_empty() {
            if let Err(e) = send_pushover_message(&pushover_token, &pushover_key, &text_clone) {
                tracing::warn!("Pushover connection-lost notification failed: {e}");
            }
        }
    })
    .await;
}

/// Send a connection-restored notification to all configured channels.
pub async fn send_connection_restored_notification(
    state: &std::sync::Arc<crate::inverter::poll::AppState>,
    host: &str,
) {
    let config = state.alert_config.lock().await.clone();
    if !config.enabled || !config.connection_lost_enabled {
        return;
    }
    let text = build_connection_restored_message(host);
    let token = config.telegram_bot_token.clone();
    let chat_id = config.telegram_chat_id.clone();
    let ntfy_topic = config.ntfy_topic.clone();
    let ntfy_server = config.ntfy_server.clone();
    let pushover_token = config.pushover_app_token.clone();
    let pushover_key = config.pushover_user_key.clone();
    let text_clone = text.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if !token.is_empty() && !chat_id.is_empty() {
            if let Err(e) = send_telegram_message(&token, &chat_id, &text_clone) {
                tracing::warn!("Telegram connection-restored notification failed: {e}");
            }
        }
        if !ntfy_topic.is_empty() {
            if let Err(e) = send_ntfy_message(&ntfy_topic, &ntfy_server, &text_clone) {
                tracing::warn!("ntfy connection-restored notification failed: {e}");
            }
        }
        if !pushover_token.is_empty() && !pushover_key.is_empty() {
            if let Err(e) = send_pushover_message(&pushover_token, &pushover_key, &text_clone) {
                tracing::warn!("Pushover connection-restored notification failed: {e}");
            }
        }
    })
    .await;
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
/// Two defences against the recurring `Telegram poll error: timeout: global`
/// that a NAT'd / containerised network path produces roughly every 5
/// minutes:
///
/// * **No connection pooling.** `max_idle_connections` and
///   `max_idle_connections_per_host` are both pinned to `0`, so every Bot API
///   call opens a fresh TCP+TLS connection. The recurring stall happens when a
///   middlebox silently reaps the TCP state of an *idle pooled* connection and
///   the poller then reuses that half-open socket (its request bytes vanish
///   until the global timeout fires). A brand-new connection is always far
///   too young for such a reaper (which acts on minutes of idle time), so the
///   stale-socket reuse path can't occur. (TCP keepalive would be the more
///   efficient fix, but ureq 3.x keeps its `TcpTransport` in a private module
///   and exposes no public socket/transport hook, so it isn't reachable from
///   application code without re-implementing the whole HTTP/1.1 transport.)
/// * **Global end-to-end timeout** ([`TELEGRAM_HTTP_TIMEOUT`]) so a genuinely
///   stalled call still can't freeze the single-threaded poll loop for the
///   OS-level TCP timeout (minutes). Repeated stalls are damped by
///   [`PollBackoff`] in the poll loop.
///
/// The agent is shared via [`OnceLock`]; with pooling disabled each call pays
/// its own TCP+TLS setup, which is negligible against the ~13s poll cadence.
fn telegram_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(TELEGRAM_HTTP_TIMEOUT)))
            // Disable connection pooling entirely — see the doc comment above.
            // This is the root-cause fix for the recurring
            // `timeout: global` polls: never reuse a (possibly reaped)
            // pooled socket.
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            // Translate 4xx/5xx to `Err(ureq::Error::StatusCode(code))` —
            // **disabled** so the callers can read Telegram's descriptive
            // error body (e.g. "Bad Request: chat not found", "Bad Request:
            // can't parse entities"). With the default `true`, ureq 3 throws
            // away the response on non-2xx and only the bare code survives,
            // which made every Telegram failure show up in the log as
            // "Telegram API 400" with no clue what actually went wrong.
            .http_status_as_error(false)
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
        Err(e) => return Err(format!("HTTP transport error: {e}")),
    };

    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        // The shared `telegram_agent` is built with
        // `http_status_as_error(false)`, so non-2xx lands here and we can
        // read Telegram's descriptive error body (e.g. "Bad Request: chat
        // not found") instead of the bare code.
        let err_body = resp
            .into_body()
            .read_to_string()
            .unwrap_or_else(|_| "<read error>".to_string());
        Err(format!("Telegram API {status} (sendMessage): {err_body}"))
    }
}

/// Send a file (HTML report) as a document via the Telegram Bot API.
///
/// `caption` is sent as the message text below the document.
/// `parse_mode`: pass `Some("HTML")` when the caption contains intentional
/// Telegram-HTML tags (e.g. `<b>`, `<i>`); pass `None` for plain text. A
/// plain-text caption MUST NOT be sent with a parse_mode — Telegram
/// otherwise tries to HTML-parse the caption and returns 400
/// "can't parse entities" the moment it sees an unescaped `<`, `>`, or
/// `&` in user-supplied content (e.g. a support-bundle description).
/// Uses `sendDocument` with `multipart/form-data`.
pub fn send_telegram_document(
    bot_token: &str,
    chat_id: &str,
    caption: &str,
    filename: &str,
    file_body: &[u8],
    parse_mode: Option<&str>,
) -> Result<(), String> {
    let url = format!("https://api.telegram.org/bot{bot_token}/sendDocument");

    let boundary = format!(
        "----HEM{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let crlf = "\r\n";

    let mut body = Vec::new();

    // chat_id field
    body.extend(format!("--{boundary}{crlf}").as_bytes());
    body.extend(format!("Content-Disposition: form-data; name=\"chat_id\"{crlf}").as_bytes());
    body.extend(crlf.as_bytes());
    body.extend(chat_id.as_bytes());
    body.extend(crlf.as_bytes());

    // document file
    body.extend(format!("--{boundary}{crlf}").as_bytes());
    body.extend(
        format!("Content-Disposition: form-data; name=\"document\"; filename=\"{filename}\"{crlf}")
            .as_bytes(),
    );
    body.extend(format!("Content-Type: application/json{crlf}").as_bytes());
    body.extend(crlf.as_bytes());
    body.extend(file_body);
    body.extend(crlf.as_bytes());

    // caption
    body.extend(format!("--{boundary}{crlf}").as_bytes());
    body.extend(format!("Content-Disposition: form-data; name=\"caption\"{crlf}").as_bytes());
    body.extend(crlf.as_bytes());
    body.extend(caption.as_bytes());
    body.extend(crlf.as_bytes());

    // parse_mode (only emitted when the caller asks for HTML formatting).
    // Omitting the field for plain-text captions prevents Telegram from
    // HTML-parsing user input and returning 400 on the first stray `<`/`&`.
    if let Some(mode) = parse_mode {
        body.extend(format!("--{boundary}{crlf}").as_bytes());
        body.extend(
            format!("Content-Disposition: form-data; name=\"parse_mode\"{crlf}").as_bytes(),
        );
        body.extend(crlf.as_bytes());
        body.extend(mode.as_bytes());
        body.extend(crlf.as_bytes());
    }

    // end
    body.extend(format!("--{boundary}--{crlf}").as_bytes());

    // The shared `telegram_agent` is built with `http_status_as_error(false)`
    // so `send` returns `Ok(response)` for any status and we can read
    // Telegram's descriptive error body on failure (e.g. "Bad Request: chat
    // not found", "Bad Request: can't parse entities"). Without that, ureq 3
    // would discard the response on non-2xx and the log would only show
    // "Telegram API 400" with no way to tell *why*.
    let resp = telegram_agent()
        .post(&url)
        .content_type(&format!("multipart/form-data; boundary={boundary}"))
        .send(body)
        .map_err(|e| format!("HTTP transport error: {e}"))?;

    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let err_body = resp
            .into_body()
            .read_to_string()
            .unwrap_or_else(|_| "<read error>".to_string());
        Err(format!(
            "Telegram API {} (sendDocument): {err_body}",
            status
        ))
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

/// Send a notification via the Pushover API.
///
/// Pushover is a paid-once-per-platform push notification service. Unlike
/// ntfy (topic-based pub/sub) it is credentialed: the app registers for an
/// app API token (<https://pushover.net/apps/build>) and each recipient has
/// a user key — two credentials, similar in spirit to Telegram's bot-token +
/// chat-id pair.
///
/// The message is sent as plain text (no `html=1`). Pushover uses the app
/// name (set at registration) as the notification title, so the alert
/// messages' built-in headers (`⚡ HEM Alert — …`) carry through as the body.
/// Priority defaults to `0` (normal) to match the flat priority of the
/// existing Telegram/ntfy channels.
///
/// Uses `ureq` (synchronous) — call from `tokio::task::spawn_blocking`.
/// Pushover accepts `application/json`, so this mirrors
/// [`send_telegram_message`] rather than form-encoding.
pub fn send_pushover_message(app_token: &str, user_key: &str, text: &str) -> Result<(), String> {
    let url = "https://api.pushover.net/1/messages.json";

    let payload = serde_json::json!({
        "token": app_token,
        "user": user_key,
        "message": text,
    });

    let body = serde_json::to_string(&payload).map_err(|e| format!("Failed to serialize: {e}"))?;

    let resp = match ureq::post(url).content_type("application/json").send(&body) {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(code)) => {
            return Err(format!(
                "Pushover API {} (check app token and user key)",
                code
            ));
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
        Err(format!("Pushover API {}: {}", status, text))
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
    msg.push_str("/report — Yesterday's full consumption report\n");
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
            { "command": "report",  "description": "Yesterday's full consumption report" },
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

/// Build the `/report` response — returns the HTML body, caption, and date
/// string for yesterday's full report, or `None` if insufficient data.
async fn build_report_reply(
    state: &crate::inverter::poll::AppState,
) -> Option<(String, String, String)> {
    let yesterday = chrono::Local::now()
        .date_naive()
        .checked_sub_signed(chrono::Duration::days(1))
        .unwrap_or_else(|| chrono::Local::now().date_naive());
    let date_str = yesterday.format("%A %d %B %Y").to_string();

    let db_guard = state.history.lock().await;
    let db = db_guard.clone();
    drop(db_guard);

    let db = db.as_ref()?;
    let rows = db.get_readings_for_date(yesterday).ok()?;
    if rows.len() < 2 {
        return None;
    }

    let html = crate::alerts::report::generate_daily_report_html(&rows, &date_str)?;
    let settings = crate::settings::Settings::load();
    let caption = crate::alerts::report::generate_daily_summary_text(&rows, &date_str, &settings)
        .unwrap_or_else(|| "📊 Daily report".to_string());

    Some((html, caption, date_str))
}

// ---------------------------------------------------------------------------
// Poll backoff
// ---------------------------------------------------------------------------

/// Exponential backoff for repeated Telegram poll failures.
///
/// Each consecutive failure grows the sleep taken *before* the next poll,
/// geometrically up to [`Self::cap`]; a single success resets it to
/// [`Self::base`]. This damps log spam and request rate during a sustained
/// outage (revoked token, broken route to `api.telegram.org`, …) while still
/// probing for recovery. It complements [`telegram_agent`]'s no-pooling fix:
/// pooling stops the common stale-socket reuse, while backoff keeps the
/// poller well-behaved when the path is genuinely down.
///
/// Pure and deterministic — it keeps no clock — so the full state machine is
/// unit tested directly.
#[derive(Debug, Clone)]
pub(crate) struct PollBackoff {
    base: Duration,
    factor: u32,
    cap: Duration,
    /// Consecutive failures so far (drives the multiplier).
    consecutive_failures: u32,
}

impl PollBackoff {
    /// Build the backoff used by the Telegram poller: base 3s, factor ×2,
    /// cap 60s. The resulting sleep *after* N consecutive failures is
    /// `3, 6, 12, 24, 48, 60, 60, …` seconds, resetting to 3 on the first
    /// success.
    pub fn new_for_telegram() -> Self {
        Self::new(Duration::from_secs(3), 2, Duration::from_secs(60))
    }

    /// Build an explicit backoff. `factor` should be ≥ 2.
    pub const fn new(base: Duration, factor: u32, cap: Duration) -> Self {
        Self {
            base,
            factor,
            cap,
            consecutive_failures: 0,
        }
    }

    /// The delay the poller should sleep right now, given its failure history.
    ///
    /// Zero failures → [`Self::base`] (the healthy cadence). Each failure
    /// multiplies by [`Self::factor`], saturating at [`Self::cap`] and holding
    /// there until a success resets it.
    pub fn current_delay(&self) -> Duration {
        let mut delay = self.base;
        for _ in 0..self.consecutive_failures {
            delay = match delay.checked_mul(self.factor) {
                Some(d) if d <= self.cap => d,
                // Overflowed or crossed the cap: hold at the cap from here on.
                _ => return self.cap,
            };
        }
        delay
    }

    /// Record a failed poll and return the (now grown) delay to sleep before
    /// retrying.
    pub fn record_failure(&mut self) -> Duration {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.current_delay()
    }

    /// Record a successful poll; resets the next delay to [`Self::base`].
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Consecutive failures currently recorded (handy for logging/tests).
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

// ---------------------------------------------------------------------------
// Poll-error severity classification
// ---------------------------------------------------------------------------

/// How loudly a Telegram poll failure should be logged.
///
/// The `getUpdates` long-poll is *designed* to hold a connection open for up
/// to 10s (`timeout=10`) waiting for updates, so a timeout surfacing from it
/// is benign — it just means "no updates this cycle, try again." With the
/// stale-socket reuse now fixed (see [`telegram_agent`]'s no-pooling config),
/// a timeout is no longer the recurring NAT-reap symptom it once was; it's a
/// rare network blip that the [`PollBackoff`] handles gracefully. Such
/// timeouts are logged at [`PollErrorSeverity::Info`] so they don't clutter
/// the WARN-level console (which is the default for both stdout and the dev
/// log ring). Anything that *isn't* a timeout — DNS failure, connection
/// refused, a bad/expired bot token (HTTP 401), … — may indicate a real
/// misconfiguration or broken path the user should know about, so it stays at
/// [`PollErrorSeverity::Warn`]. Both severities still feed the backoff
/// counter, since either way the poll failed and we want to ease off if it
/// keeps happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollErrorSeverity {
    /// Benign/expected (a timeout) — log at INFO.
    Info,
    /// May need user attention (DNS, auth, refused, …) — log at WARN.
    Warn,
}

/// Classify a Telegram poll error for logging. See [`PollErrorSeverity`].
///
/// Pure (and `&self`-free) so the full classification table is unit-tested
/// directly — there's no other way to exercise logging-level decisions.
pub(crate) fn poll_error_severity(err: &ureq::Error) -> PollErrorSeverity {
    // Every `Timeout` variant (Global, Connect, RecvResponse, …) is a benign
    // "the call didn't complete in time" — expected for a long-poll and
    // handled by the backoff. All other errors are surfaced as WARN.
    if matches!(err, ureq::Error::Timeout(_)) {
        PollErrorSeverity::Info
    } else {
        PollErrorSeverity::Warn
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
        // Exponential backoff on repeated poll failures. Starts at the healthy
        // 3s cadence and only grows after consecutive transport failures.
        let mut backoff = PollBackoff::new_for_telegram();
        loop {
            tokio::time::sleep(backoff.current_delay()).await;

            let config = state.alert_config.lock().await.clone();
            if !config.enabled || config.telegram_bot_token.is_empty() {
                // Token removed/alerts disabled — re-register the command menu
                // the next time a token is configured. This isn't a network
                // failure, so reset the backoff: the first real poll after
                // re-enabling should use the base interval, not whatever the
                // last outage grew it to.
                commands_registered = false;
                backoff.record_success();
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

            // Run the HTTP poll on a blocking thread so we don't stall the
            // async runtime. Transport errors come back as the typed
            // `ureq::Error` so the loop can classify them (benign timeout vs.
            // genuine failure) and grow the backoff; a successful poll (even
            // an empty one) resets it.
            let poll_outcome = tokio::task::spawn_blocking(
                move || -> Result<Vec<(i64, i64, String)>, ureq::Error> {
                    let url = format!(
                        "https://api.telegram.org/bot{}/getUpdates?offset={}&timeout=10",
                        poll_token, cur_offset
                    );
                    match telegram_agent().get(&url).call() {
                        Ok(r) => {
                            let body = r.into_body().read_to_string().unwrap_or_default();
                            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&body) {
                                let mut msgs = Vec::new();
                                if let Some(results) = data["result"].as_array() {
                                    for update in results {
                                        let update_id = update["update_id"].as_i64().unwrap_or(0);
                                        if let Some(msg) = update.get("message") {
                                            let chat_id = msg["chat"]["id"].as_i64().unwrap_or(0);
                                            let text =
                                                msg["text"].as_str().unwrap_or("").to_string();
                                            msgs.push((update_id, chat_id, text));
                                        }
                                    }
                                }
                                Ok(msgs)
                            } else {
                                // A body we couldn't parse isn't a transport
                                // failure — map it to an empty success so it
                                // doesn't trigger backoff.
                                Ok(Vec::new())
                            }
                        }
                        Err(e) => Err(e),
                    }
                },
            )
            .await;

            let updates = match poll_outcome {
                Ok(Ok(updates)) => {
                    backoff.record_success();
                    updates
                }
                Ok(Err(err)) => {
                    // A poll failed. Grow the backoff either way (we want to
                    // ease off if it keeps happening), but log at a level
                    // matching the severity: a benign timeout is INFO (the
                    // long-poll simply returned no updates), while anything
                    // else (DNS, auth, refused, …) is WARN since it may need
                    // user attention.
                    let delay = backoff.record_failure();
                    let consecutive = backoff.consecutive_failures();
                    match poll_error_severity(&err) {
                        PollErrorSeverity::Info => tracing::debug!(
                            "Telegram poll timed out (benign — long-poll returned \
                             no updates); retrying in ~{delay:?} \
                             (consecutive timeouts: {consecutive})"
                        ),
                        PollErrorSeverity::Warn => tracing::warn!(
                            "Telegram poll error: {err}; backing off after {consecutive} \
                             consecutive failure(s), next attempt in ~{delay:?}"
                        ),
                    }
                    Vec::new()
                }
                Err(join_err) => {
                    // The spawn_blocking task itself failed (panic/cancel).
                    // This is never benign — a panic in the poll task signals a
                    // bug and warrants attention.
                    let delay = backoff.record_failure();
                    let consecutive = backoff.consecutive_failures();
                    tracing::warn!(
                        "Telegram poll task failed: {join_err}; backing off after {consecutive} \
                         consecutive failure(s), next attempt in ~{delay:?}"
                    );
                    Vec::new()
                }
            };

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
                    "report" => {
                        match build_report_reply(&state).await {
                            Some((html_body, caption, date_str)) => {
                                let cid_str = chat_id.to_string();
                                let token_c = token.clone();
                                let filename = format!("hem-report-{}.html", date_str);
                                tokio::task::spawn_blocking(move || {
                                    if let Err(e) = crate::alerts::send_telegram_document(
                                        &token_c,
                                        &cid_str,
                                        &caption,
                                        &filename,
                                        html_body.as_bytes(),
                                        Some("HTML"),
                                    ) {
                                        tracing::warn!("Telegram /report failed: {e}");
                                    }
                                });
                                continue; // already sent via document, skip text reply
                            }
                            None => "⚠️ Not enough data for yesterday's report yet.".to_string(),
                        }
                    }
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
            inverter_temp_min: 8.0,
            inverter_temp_max: 60.0,
            soc_min: 10,
            soc_max: 95,
            grid_offline_enabled: false,
            inverter_trip_enabled: false,
            battery_over_temp_enabled: false,
            connection_lost_enabled: false,
            solar_clipping_enabled: false,
            solar_clipping_ceiling_w: 0,
            ntfy_topic: String::new(),
            ntfy_server: "https://ntfy.sh".to_string(),
            pushover_app_token: String::new(),
            pushover_user_key: String::new(),
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
    fn test_no_alerts_when_no_channel_configured() {
        // All three channels empty → evaluate_alerts must short-circuit.
        let mut config = alerts_config();
        config.telegram_bot_token.clear();
        config.telegram_chat_id.clear();
        config.ntfy_topic.clear();
        config.pushover_app_token.clear();
        config.pushover_user_key.clear();
        let alerts = evaluate_alerts(&make_snapshot(), &config);
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_pushover_alone_enables_gate() {
        // Pushover is the only configured channel — alerts must still fire.
        // make_snapshot() has battery_temperature=35, batt_temp_max=45 → no
        // temp alert, so drive a SOC-high trigger instead.
        let mut snap = make_snapshot();
        snap.soc = 99;
        let mut config = alerts_config();
        config.telegram_bot_token.clear();
        config.telegram_chat_id.clear();
        config.ntfy_topic.clear();
        config.pushover_app_token = "app-token".to_string();
        config.pushover_user_key = "user-key".to_string();
        let alerts = evaluate_alerts(&snap, &config);
        assert!(
            alerts.contains(&AlertType::BatterySocHigh),
            "pushover-only config should still evaluate thresholds"
        );
    }

    #[test]
    fn test_ntfy_alone_enables_gate() {
        // ntfy is the only configured channel — alerts must still fire.
        let mut snap = make_snapshot();
        snap.soc = 99;
        let mut config = alerts_config();
        config.telegram_bot_token.clear();
        config.telegram_chat_id.clear();
        config.ntfy_topic = "hem-test".to_string();
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::BatterySocHigh));
    }

    #[test]
    fn test_pushover_missing_user_key_blocks_gate() {
        // Only the app token is set (no user key) — not a valid Pushover
        // config, so the gate must short-circuit.
        let mut config = alerts_config();
        config.telegram_bot_token.clear();
        config.telegram_chat_id.clear();
        config.pushover_app_token = "app-token".to_string();
        // user_key stays empty
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
    fn test_battery_temp_ignores_nan() {
        let mut snap = make_snapshot();
        snap.battery_temperature = f32::NAN;
        let alerts = evaluate_alerts(&snap, &alerts_config());
        assert!(!alerts.contains(&AlertType::BatteryTempHigh));
        assert!(!alerts.contains(&AlertType::BatteryTempLow));
    }

    #[test]
    fn test_inverter_temp_high_triggers() {
        let mut snap = make_snapshot();
        snap.inverter_temperature = 65.0;
        let alerts = evaluate_alerts(&snap, &alerts_config());
        assert!(alerts.contains(&AlertType::InverterTempHigh));
    }

    #[test]
    fn test_inverter_temp_low_triggers() {
        let mut snap = make_snapshot();
        snap.inverter_temperature = 5.0;
        let alerts = evaluate_alerts(&snap, &alerts_config());
        assert!(alerts.contains(&AlertType::InverterTempLow));
    }

    #[test]
    fn test_inverter_temp_no_alert_when_ok() {
        let snap = make_snapshot();
        let alerts = evaluate_alerts(&snap, &alerts_config());
        assert!(!alerts.contains(&AlertType::InverterTempHigh));
        assert!(!alerts.contains(&AlertType::InverterTempLow));
    }

    #[test]
    fn test_inverter_temp_ignores_nan() {
        let mut snap = make_snapshot();
        snap.inverter_temperature = f32::NAN;
        let alerts = evaluate_alerts(&snap, &alerts_config());
        assert!(!alerts.contains(&AlertType::InverterTempHigh));
        assert!(!alerts.contains(&AlertType::InverterTempLow));
    }

    #[test]
    fn test_inverter_temp_threshold_zero_disables() {
        let mut snap = make_snapshot();
        snap.inverter_temperature = 65.0;
        let mut config = alerts_config();
        config.inverter_temp_min = 0.0;
        config.inverter_temp_max = 0.0;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(!alerts.contains(&AlertType::InverterTempHigh));
        assert!(!alerts.contains(&AlertType::InverterTempLow));
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
    fn test_grid_offline_does_not_include_inverter_trip() {
        let mut snap = make_snapshot();
        snap.inverter_trip = true;
        let mut config = alerts_config();
        config.grid_offline_enabled = true;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(!alerts.contains(&AlertType::GridOffline));
    }

    #[test]
    fn test_inverter_trip() {
        let mut snap = make_snapshot();
        snap.inverter_trip = true;
        let mut config = alerts_config();
        config.inverter_trip_enabled = true;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::InverterTrip));
    }

    #[test]
    fn test_inverter_trip_disabled() {
        let mut snap = make_snapshot();
        snap.inverter_trip = true;
        let alerts = evaluate_alerts(&snap, &alerts_config());
        assert!(!alerts.contains(&AlertType::InverterTrip));
    }

    #[test]
    fn test_grid_offline_when_grid_online_false() {
        let mut snap = make_snapshot();
        snap.grid_online = false;
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

    // ================================================================
    // PollBackoff — exponential backoff for repeated Telegram poll failures
    // ================================================================

    #[test]
    fn test_backoff_starts_at_base() {
        // With no failures recorded the poller sleeps the healthy base cadence.
        let b = PollBackoff::new_for_telegram();
        assert_eq!(b.current_delay(), Duration::from_secs(3));
        assert_eq!(b.consecutive_failures(), 0);
    }

    #[test]
    fn test_backoff_sequence_after_consecutive_failures() {
        // Telegram defaults: base=3s, factor=2, cap=60s.
        // Expected next-delays: 3, 6, 12, 24, 48, 60, 60, 60, …
        let mut b = PollBackoff::new_for_telegram();
        assert_eq!(b.current_delay(), Duration::from_secs(3)); // 0 failures
        let expected = [
            (1u32, Duration::from_secs(6)),
            (2, Duration::from_secs(12)),
            (3, Duration::from_secs(24)),
            (4, Duration::from_secs(48)),
            (5, Duration::from_secs(60)),
            (6, Duration::from_secs(60)),
            (7, Duration::from_secs(60)),
        ];
        for (failures, want) in expected {
            let got = b.record_failure();
            assert_eq!(
                got, want,
                "delay after {failures} consecutive failures should be {want:?}, got {got:?}"
            );
            assert_eq!(b.consecutive_failures(), failures);
            assert_eq!(
                b.current_delay(),
                want,
                "current_delay must match record_failure"
            );
        }
    }

    #[test]
    fn test_backoff_current_delay_is_pure() {
        // current_delay() must not mutate state — repeated calls are identical.
        let mut b = PollBackoff::new_for_telegram();
        b.record_failure();
        b.record_failure();
        let first = b.current_delay();
        let second = b.current_delay();
        let third = b.current_delay();
        assert_eq!(first, Duration::from_secs(12));
        assert_eq!(first, second);
        assert_eq!(second, third);
        assert_eq!(
            b.consecutive_failures(),
            2,
            "current_delay must not bump the counter"
        );
    }

    #[test]
    fn test_backoff_caps_and_holds() {
        // Far past the cap the delay stays pinned at the cap, never above.
        let mut b = PollBackoff::new_for_telegram();
        for _ in 0..50 {
            let d = b.record_failure();
            assert!(
                d <= Duration::from_secs(60),
                "delay {d:?} exceeded the 60s cap"
            );
        }
        assert_eq!(b.current_delay(), Duration::from_secs(60));
    }

    #[test]
    fn test_backoff_success_resets_to_base() {
        let mut b = PollBackoff::new_for_telegram();
        for _ in 0..5 {
            b.record_failure();
        }
        assert!(b.current_delay() > Duration::from_secs(3));
        b.record_success();
        assert_eq!(b.consecutive_failures(), 0);
        assert_eq!(b.current_delay(), Duration::from_secs(3));
    }

    #[test]
    fn test_backoff_success_resets_not_decrements() {
        // A single success fully resets the sequence — it does not decrement.
        let mut b = PollBackoff::new_for_telegram();
        b.record_failure(); // -> 6s
        b.record_failure(); // -> 12s
        assert_eq!(b.current_delay(), Duration::from_secs(12));
        b.record_success();
        // The next failure starts over from base × factor.
        assert_eq!(b.record_failure(), Duration::from_secs(6));
    }

    #[test]
    fn test_backoff_custom_params() {
        // base=1s, factor=3, cap=10s → 1, 3, 9, 10(capped), 10, …
        let mut b = PollBackoff::new(Duration::from_secs(1), 3, Duration::from_secs(10));
        assert_eq!(b.current_delay(), Duration::from_secs(1));
        assert_eq!(b.record_failure(), Duration::from_secs(3));
        assert_eq!(b.record_failure(), Duration::from_secs(9));
        assert_eq!(b.record_failure(), Duration::from_secs(10)); // 9×3=27 > cap
        assert_eq!(b.record_failure(), Duration::from_secs(10));
    }

    #[test]
    fn test_backoff_exact_cap_value_is_kept() {
        // base=2s, factor=2, cap=8s → 2, 4, 8, 8. A delay landing exactly on
        // the cap must be kept as-is, not prematurely clamped from below.
        let mut b = PollBackoff::new(Duration::from_secs(2), 2, Duration::from_secs(8));
        assert_eq!(b.current_delay(), Duration::from_secs(2));
        assert_eq!(b.record_failure(), Duration::from_secs(4));
        assert_eq!(b.record_failure(), Duration::from_secs(8));
        assert_eq!(b.record_failure(), Duration::from_secs(8));
    }

    #[test]
    fn test_backoff_large_factor_does_not_panic() {
        // A factor that would overflow Duration via naive multiplication must
        // be caught (checked_mul) and saturate to the cap with no panic.
        let mut b = PollBackoff::new(Duration::from_secs(1), u32::MAX, Duration::from_secs(5));
        assert_eq!(b.record_failure(), Duration::from_secs(5));
        assert_eq!(b.record_failure(), Duration::from_secs(5));
        assert_eq!(b.current_delay(), Duration::from_secs(5));
    }

    #[test]
    fn test_backoff_failure_counter_never_overflows() {
        // saturating_add guards the counter; a long outage can't overflow it.
        let mut b = PollBackoff::new_for_telegram();
        for _ in 0..1000 {
            b.record_failure();
        }
        assert!(b.consecutive_failures() >= 1000);
        assert_eq!(b.current_delay(), Duration::from_secs(60));
    }

    // ================================================================
    // poll_error_severity — benign timeout vs. genuine failure
    // ================================================================

    #[test]
    fn test_poll_error_severity_all_timeouts_are_benign() {
        // Every Timeout variant is a benign "the call didn't complete in time",
        // expected for the getUpdates long-poll. The production matcher uses a
        // wildcard (`matches!(err, ureq::Error::Timeout(_))`), so any future
        // variant is handled correctly too; this loop just pins the current
        // enum's intent.
        for timeout in [
            ureq::Timeout::Global,
            ureq::Timeout::PerCall,
            ureq::Timeout::Resolve,
            ureq::Timeout::Connect,
            ureq::Timeout::SendRequest,
            ureq::Timeout::SendBody,
            ureq::Timeout::Await100,
            ureq::Timeout::RecvResponse,
            ureq::Timeout::RecvBody,
        ] {
            assert_eq!(
                poll_error_severity(&ureq::Error::Timeout(timeout)),
                PollErrorSeverity::Info,
                "Timeout variant {:?} should be benign (Info)",
                timeout
            );
        }
    }

    #[test]
    fn test_poll_error_severity_global_timeout_is_info() {
        // The specific case from the bug report: `timeout: global`.
        assert_eq!(
            poll_error_severity(&ureq::Error::Timeout(ureq::Timeout::Global)),
            PollErrorSeverity::Info
        );
    }

    #[test]
    fn test_poll_error_severity_genuine_errors_warn() {
        // Non-timeout failures may indicate misconfiguration or a broken path.
        assert_eq!(
            poll_error_severity(&ureq::Error::HostNotFound),
            PollErrorSeverity::Warn
        );
        assert_eq!(
            poll_error_severity(&ureq::Error::ConnectionFailed),
            PollErrorSeverity::Warn
        );
        // HTTP 401 = bad/expired bot token — definitely not benign.
        assert_eq!(
            poll_error_severity(&ureq::Error::StatusCode(401)),
            PollErrorSeverity::Warn
        );
        assert_eq!(
            poll_error_severity(&ureq::Error::StatusCode(500)),
            PollErrorSeverity::Warn
        );
    }

    #[test]
    fn test_poll_error_severity_io_error_warns() {
        // A raw transport I/O error (connection reset, broken pipe, …) is a
        // genuine failure, not a long-poll timeout.
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset by peer");
        assert_eq!(
            poll_error_severity(&ureq::Error::Io(io_err)),
            PollErrorSeverity::Warn
        );
    }

    // ================================================================
    // telegram_agent — no-pooling + global timeout config
    // ================================================================

    #[test]
    fn test_telegram_agent_disables_connection_pooling() {
        // Root-cause fix for `timeout: global`: pooled (possibly reaped)
        // sockets must never be reused, so both pool limits are pinned to 0.
        let cfg = telegram_agent().config();
        assert_eq!(
            cfg.max_idle_connections(),
            0,
            "global pool must be disabled"
        );
        assert_eq!(
            cfg.max_idle_connections_per_host(),
            0,
            "per-host pool must be disabled"
        );
    }

    #[test]
    fn test_telegram_agent_disables_http_status_as_error() {
        // Without `http_status_as_error(false)`, ureq 3 translates 4xx/5xx
        // to `Err(ureq::Error::StatusCode(u16))` and **discards the response
        // body** — so Telegram errors like "Bad Request: chat not found"
        // or "Bad Request: can't parse entities" never reach our error
        // string, and every failure shows up in the log as a bare
        // "Telegram API 400" with no clue why. Pin the config to false so
        // `send` returns `Ok(response)` for any status and callers can read
        // the descriptive body.
        let cfg = telegram_agent().config();
        assert!(
            !cfg.http_status_as_error(),
            "http_status_as_error must be disabled so non-2xx response bodies are readable"
        );
    }

    #[test]
    fn test_telegram_agent_global_timeout_is_set() {
        let cfg = telegram_agent().config();
        assert_eq!(
            cfg.timeouts().global,
            Some(Duration::from_secs(TELEGRAM_HTTP_TIMEOUT))
        );
    }

    #[test]
    fn test_telegram_agent_is_shared_singleton() {
        // OnceLock: every caller gets the same agent (one config, one pool).
        let a = telegram_agent();
        let b = telegram_agent();
        assert!(std::ptr::eq(a, b));
    }
}
