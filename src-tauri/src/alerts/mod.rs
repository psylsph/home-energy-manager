//! Alert evaluation engine and Telegram notification sender.
//!
//! Evaluates inverter snapshot against user-configured thresholds after
//! sanitization, then sends notifications via the Telegram Bot API.
//!
//! Also handles daily consumption report generation and sending.

pub mod report;
pub mod whatsapp;
pub mod whatsapp_store;

use std::collections::HashMap;
use std::time::Instant;

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
    SolarClipping,
    PvStringLoss,
    GridOffline,
    BatteryOverTemp,
}

impl AlertType {
    /// Human-readable name for the alert.
    pub fn human_name(&self) -> &'static str {
        match self {
            Self::BatteryTempHigh => "Battery Temperature High",
            Self::BatteryTempLow => "Battery Temperature Low",
            Self::BatterySocHigh => "Battery SOC High",
            Self::BatterySocLow => "Battery SOC Low",
            Self::SolarClipping => "Solar Clipping",
            Self::PvStringLoss => "PV String Circuit Loss",
            Self::GridOffline => "Grid Offline",
            Self::BatteryOverTemp => "Battery Over-Temperature",
        }
    }
}

// ---------------------------------------------------------------------------
// Debounce tracker
// ---------------------------------------------------------------------------

/// Per-alert-type debounce tracker to prevent notification floods.
#[derive(Debug)]
pub struct AlertDebounce {
    /// Map from alert type to the last time it was sent.
    last_sent: HashMap<AlertType, Instant>,
    /// Set of alert types currently in an active (fired) state.
    active: std::collections::HashSet<AlertType>,
}

impl AlertDebounce {
    pub fn new() -> Self {
        Self {
            last_sent: HashMap::new(),
            active: std::collections::HashSet::new(),
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
        let triggered_set: std::collections::HashSet<_> = currently_triggered.iter().copied().collect();
        let cleared: Vec<_> = self.active.difference(&triggered_set).copied().collect();
        for c in &cleared {
            self.active.remove(c);
        }
        cleared
    }

    /// Number of entries in the debounce map (for API display).
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
    let has_telegram =
        !config.telegram_bot_token.is_empty() && !config.telegram_chat_id.is_empty();
    if !has_telegram {
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

    // Solar clipping: solar output near inverter's max AC capacity
    if config.solar_clipping_enabled {
        let max_ac = snapshot.max_ac_power_w;
        if max_ac > 0 && (snapshot.solar_power as u32) > max_ac.saturating_mul(95) / 100 {
            alerts.push(AlertType::SolarClipping);
        }
    }

    // PV string loss: one string near zero while the other produces,
    // or both near zero while total solar > 100 W.
    // Guards against false positives: if a string has voltage > 50 V it
    // is clearly connected and should not trigger, even if power is low.
    if config.pv_string_loss_enabled {
        let solar = snapshot.solar_power;
        let pv1 = snapshot.pv1_power.unsigned_abs() as i32;
        let pv2 = snapshot.pv2_power.unsigned_abs() as i32;
        let pv1_v = snapshot.pv1_voltage;
        let pv2_v = snapshot.pv2_voltage;
        let pv1_near_zero = pv1 < 10 && pv1_v < 50.0;
        let pv2_near_zero = pv2 < 10 && pv2_v < 50.0;

        if (solar > 100 && pv1_near_zero && pv2_near_zero)
            || (pv1 > 50 && pv2_near_zero)
            || (pv2 > 50 && pv1_near_zero)
        {
            alerts.push(AlertType::PvStringLoss);
        }
    }

    // Grid offline
    if config.grid_offline_enabled && snapshot.grid_loss {
        alerts.push(AlertType::GridOffline);
    }

    // Battery over-temperature
    if config.battery_over_temp_enabled && snapshot.battery_over_temp {
        alerts.push(AlertType::BatteryOverTemp);
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

/// Send a notification via the Telegram Bot API.
///
/// Uses `ureq` (synchronous) — call from `tokio::task::spawn_blocking`.
pub fn send_telegram_message(
    bot_token: &str,
    chat_id: &str,
    text: &str,
) -> Result<(), String> {
    let url = format!(
        "https://api.telegram.org/bot{}/sendMessage",
        bot_token
    );

    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
    });

    let body =
        serde_json::to_string(&payload).map_err(|e| format!("Failed to serialize: {e}"))?;

    let resp = match ureq::post(&url)
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
    msg.push_str(&format!("🔋 Battery: <b>{} W</b>  (SOC: <b>{}%</b>)\n", snapshot.battery_power, snapshot.soc));
    msg.push_str(&format!("⚡ Grid: <b>{} W</b>\n", snapshot.grid_power));
    msg.push_str(&format!(
        "🌡️ Battery temp: {:.1}°C  |  Inverter: {:.1}°C\n",
        snapshot.battery_temperature, snapshot.inverter_temperature
    ));
    msg.push_str(&format!(
        "📶 Grid: {}\n",
        if snapshot.grid_online { "🟢 Online" } else { "🔴 Offline" }
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

/// Spawns a background task that polls Telegram for /status commands and
/// replies with the current inverter snapshot.
///
/// The task reads `state.alert_config` on each cycle so config changes
/// (token updates) take effect without restart.
pub fn spawn_telegram_poller(state: std::sync::Arc<crate::inverter::poll::AppState>) {
    tracing::debug!("Telegram poller started");
    tokio::spawn(async move {
        let mut offset: i64 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;

            let config = state.alert_config.lock().await.clone();
            if !config.enabled || config.telegram_bot_token.is_empty() {
                continue;
            }
            let token = config.telegram_bot_token.clone();
            let cur_offset = offset;
            let poll_token = token.clone();

            // Run the HTTP poll on a blocking thread so we don't stall the async runtime
            let updates = tokio::task::spawn_blocking(move || -> Vec<(i64, i64, String)> {
                let url = format!(
                    "https://api.telegram.org/bot{}/getUpdates?offset={}&timeout=10",
                    poll_token, cur_offset
                );
                let result = ureq::get(&url).call();
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
            }).await.unwrap_or_default();

            for (update_id, chat_id, text) in &updates {
                if *update_id >= offset {
                    offset = update_id + 1;
                }
                if *chat_id != 0
                    && (text.eq_ignore_ascii_case("/status") || text.eq_ignore_ascii_case("/start"))
                {
                        let snapshot = state.latest_snapshot.lock().await;
                        let reply = if let Some(ref snap) = *snapshot {
                            build_status_message(snap)
                        } else {
                            "⚠️ No inverter data available yet. Waiting for connection...".to_string()
                        };
                        drop(snapshot);

                        let cid_str = chat_id.to_string();
                        let token_c = token.clone();
                        tracing::info!("Telegram: replying to /status for chat {}", chat_id);
                        tokio::task::spawn_blocking(move || {
                            if let Err(e) = send_telegram_message(&token_c, &cid_str, &reply) {
                                tracing::warn!("Telegram reply failed: {e}");
                            }
                        });
                    }
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
            solar_clipping_enabled: false,
            pv_string_loss_enabled: false,
            grid_offline_enabled: false,
            battery_over_temp_enabled: false,
            whatsapp_recipient: String::new(),
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
    fn test_solar_clipping() {
        let mut snap = make_snapshot();
        snap.solar_power = 5800;
        snap.max_ac_power_w = 6000;
        let mut config = alerts_config();
        config.solar_clipping_enabled = true;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::SolarClipping));
    }

    #[test]
    fn test_solar_no_clipping_when_under() {
        let mut snap = make_snapshot();
        snap.solar_power = 4000;
        snap.max_ac_power_w = 6000;
        let mut config = alerts_config();
        config.solar_clipping_enabled = true;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(!alerts.contains(&AlertType::SolarClipping));
    }

    #[test]
    fn test_pv_string_loss_one_dead() {
        let mut snap = make_snapshot();
        snap.pv1_power = 3000;
        snap.pv2_power = 0;
        snap.solar_power = 3000;
        let mut config = alerts_config();
        config.pv_string_loss_enabled = true;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::PvStringLoss));
    }

    #[test]
    fn test_pv_string_loss_both_dead_solar_producing() {
        let mut snap = make_snapshot();
        snap.pv1_power = 0;
        snap.pv2_power = 0;
        snap.solar_power = 200;
        let mut config = alerts_config();
        config.pv_string_loss_enabled = true;
        let alerts = evaluate_alerts(&snap, &config);
        assert!(alerts.contains(&AlertType::PvStringLoss));
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
}
