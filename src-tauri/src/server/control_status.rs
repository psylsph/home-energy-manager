//! Cached battery operating summary for authenticated integrations.
//!
//! Mode, measured activity, controller intent and restrictions are orthogonal.
//! In particular, a queued force action is not proof of charging/exporting.
//! Slot evaluation uses the inverter clock (including the raw inverse pause
//! window), never the HTTP client's or server's timezone.

use crate::inverter::{
    model::{BatteryMode, BatteryState, DeviceType, InverterSnapshot, ScheduleSlot},
    poll::{AppState, ConnectionState},
    state_machines::{
        export_window_contains, inverter_minute_of_day, TimedExportConfig, TimedExportState,
    },
};
use axum::{
    extract::State,
    http::{header::CACHE_CONTROL, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ForceKind {
    Charge,
    Discharge,
}

impl ForceKind {
    fn code(self) -> &'static str {
        match self {
            Self::Charge => "force_charge",
            Self::Discharge => "force_discharge",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Charge => "Force Charge",
            Self::Discharge => "Force Discharge",
        }
    }
}

struct ForceWindow {
    kind: ForceKind,
    started_at_ms: i64,
    end_ms: Option<i64>,
}

#[derive(Default)]
struct Context {
    force: Option<ForceWindow>,
    export_config: TimedExportConfig,
    export_state: TimedExportState,
}

pub async fn get_status(State(state): State<Arc<AppState>>) -> Response {
    // Short, memory-only locks. Do not acquire force_action_lock: the poll
    // loop can hold that lock while performing slow restoration writes.
    let force = {
        let charge = state.force_charge_revert.lock().await;
        let discharge = state.force_discharge_revert.lock().await;
        charge
            .as_ref()
            .map(|r| ForceWindow {
                kind: ForceKind::Charge,
                started_at_ms: r.started_at_ms,
                end_ms: r.force_charge_slot_end_ms,
            })
            .or_else(|| {
                discharge.as_ref().map(|r| ForceWindow {
                    kind: ForceKind::Discharge,
                    started_at_ms: r.started_at_ms,
                    end_ms: r.force_discharge_slot_end_ms,
                })
            })
    };
    let export_config = state.timed_export_config.lock().await.clone();
    let export_state = state.timed_export_state.lock().await.clone();
    let conn = state.connection_state.lock().await.clone();
    let interval = state.settings.lock().await.interval_secs;
    // Build under the guard (mirroring mini_status) instead of cloning the
    // whole snapshot; build_status is synchronous and never awaits.
    let snapshot = state.latest_snapshot.lock().await;
    let value = build_status(
        snapshot.as_ref(),
        conn,
        interval,
        &Context {
            force,
            export_config,
            export_state,
        },
        Utc::now().timestamp_millis(),
    );
    drop(snapshot);
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn mode_label(mode: BatteryMode) -> &'static str {
    match mode {
        BatteryMode::Eco => "Eco",
        BatteryMode::EcoPaused => "Eco Paused",
        BatteryMode::TimedDemand => "Timed Demand",
        BatteryMode::TimedExport => "Timed Export",
        BatteryMode::ExportPaused => "Export Paused",
        BatteryMode::Unknown => "Unknown",
    }
}

fn export_phase(state: &TimedExportState) -> &'static str {
    match state {
        TimedExportState::Off => "off",
        TimedExportState::Configured => "configured",
        TimedExportState::Entering { .. } => "entering",
        TimedExportState::Active => "active",
        TimedExportState::Exiting { .. } => "exiting",
        TimedExportState::BlockedByPause => "blocked_by_pause",
        TimedExportState::Error { .. } => "error",
    }
}

fn reported_phase(phase: &str) -> &str {
    if phase.is_empty() {
        "unknown"
    } else {
        phase
    }
}

fn calibration_phase(stage: u8) -> &'static str {
    match stage {
        0 => "off",
        1 => "discharging",
        2 => "setting_lower_limit",
        3 => "charging",
        4 => "setting_upper_limit",
        5 => "balancing",
        6 => "setting_full_capacity",
        7 => "finished",
        _ => "unknown",
    }
}

fn maintenance_phase(mode: u8) -> &'static str {
    match mode {
        0 => "off",
        1 => "discharging",
        2 => "charging",
        3 => "standby",
        _ => "unknown",
    }
}

fn window(slots: &[ScheduleSlot], minute: Option<u16>) -> Option<bool> {
    if slots
        .iter()
        .filter(|s| s.enabled)
        .any(|s| s.start_hour > 23 || s.end_hour > 23 || s.start_minute > 59 || s.end_minute > 59)
    {
        return None;
    }
    if !slots.iter().any(ScheduleSlot::is_configured) {
        return Some(false);
    }
    minute.map(|m| export_window_contains(slots, m))
}

fn schedule(armed: bool, in_window: Option<bool>, performing: bool) -> &'static str {
    if !armed {
        "off"
    } else {
        match in_window {
            None => "unknown",
            Some(true) if performing => "active",
            _ => "armed",
        }
    }
}

/// `now_ms` is injected. Readings older than three polling intervals (minimum
/// 60s), or >5s in the future, are not presented as live. Countdown rounding is
/// upwards so an unexpired window never says zero minutes remaining.
fn build_status(
    snapshot: Option<&InverterSnapshot>,
    conn: ConnectionState,
    interval_secs: u64,
    ctx: &Context,
    now_ms: i64,
) -> Value {
    let observed_at = snapshot
        .and_then(|s| DateTime::<Utc>::from_timestamp(s.timestamp, 0))
        .map(|t| t.to_rfc3339());
    let age_s = snapshot.map(|s| (now_ms / 1000).saturating_sub(s.timestamp));
    let stale_after_s = interval_secs.saturating_mul(3).max(60).min(i64::MAX as u64) as i64;
    // "Stale" is only about a reading's age; "never observed" is carried by
    // observed_at: null / ok: false rather than a misleading stale flag.
    let stale = age_s.is_some_and(|age| age > stale_after_s || age < -5);
    let available = conn == ConnectionState::Connected && !stale && snapshot.is_some();
    let mut value = json!({
        "ok":available, "summary":"Awaiting first inverter reading", "mode":"unknown", "activity":"unavailable",
        "control_source":"unknown", "control_phase":"unknown", "remaining_minutes":null,
        "schedules":{"charge":"unknown","export":"unknown","demand_discharge":"unknown"},
        "automation":null, "conditions":[], "calibration":null, "maintenance":null,
        "quick_action":null, "limits":null,
        "connection":conn, "stale":stale, "observed_at":observed_at,
        "age_seconds":age_s.map(|age| age.max(0)), "stale_after_seconds":stale_after_s,
    });
    if !available {
        value["summary"] = json!(match conn {
            ConnectionState::Disconnected => "Disconnected — current battery status unavailable",
            ConnectionState::Reconnecting => "Reconnecting — current battery status unavailable",
            ConnectionState::Connected if snapshot.is_none() => "Awaiting first inverter reading",
            ConnectionState::Connected =>
                "Stale inverter reading — current battery status unavailable",
        });
        return value;
    }
    let s = snapshot.expect("available requires a snapshot");
    let known_device = !matches!(
        s.device_type,
        DeviceType::Unknown(_) | DeviceType::PvInverter
    );
    let mode = if known_device {
        s.battery_mode
    } else {
        BatteryMode::Unknown
    };
    let activity = if known_device {
        match s.battery_state {
            BatteryState::Charging => "charging",
            BatteryState::Discharging => "discharging",
            BatteryState::Idle => "idle",
        }
    } else {
        "unavailable"
    };
    let minute = inverter_minute_of_day(s);
    let charge_window = window(&s.charge_slots, minute);
    let discharge_window = window(&s.discharge_slots, minute);
    let pause_window = window(std::slice::from_ref(&s.battery_pause_slot), minute);
    let charge_paused = matches!(s.battery_pause_mode, 1 | 3) && pause_window == Some(true);
    let discharge_paused = matches!(s.battery_pause_mode, 2 | 3) && pause_window == Some(true);
    let charge = schedule(
        s.enable_charge,
        charge_window,
        activity == "charging" && !charge_paused,
    );
    // A legacy Timed Demand discharge window is not forced grid export.
    let export_slots = if ctx.export_config.schedule_enabled {
        &ctx.export_config.slots[..]
    } else {
        &s.discharge_slots[..]
    };
    let export = schedule(
        ctx.export_config.schedule_enabled || (s.enable_discharge && s.battery_power_mode == 0),
        window(export_slots, minute),
        s.enable_discharge
            && s.battery_power_mode == 0
            && activity == "discharging"
            && !discharge_paused,
    );
    let demand = if matches!(s.battery_pause_mode, 2 | 3) {
        schedule(true, pause_window.map(|v| !v), activity == "discharging")
    } else {
        schedule(
            s.enable_discharge && s.battery_power_mode == 1,
            discharge_window,
            activity == "discharging",
        )
    };
    value["mode"] = json!(mode);
    value["activity"] = json!(activity);
    if known_device {
        value["schedules"] = json!({"charge":charge,"export":export,"demand_discharge":demand});
    }

    value["automation"] = json!({
        "charging_mode":s.charging_mode,
        "cosy":{"enabled":s.cosy_enabled,"active":s.cosy_active,"phase":if s.cosy_active {"active"} else if s.cosy_enabled {"waiting"} else {"off"}},
        "agile":{"enabled":s.agile_enabled,"active":s.agile_active,"phase":reported_phase(&s.agile_state),"scope":s.agile_scope},
        "adaptive":{"enabled":s.adaptive_charge_enabled,"phase":reported_phase(&s.adaptive_charge_state),"period":s.adaptive_charge_period},
        "winter":{"active":s.auto_winter_active},
        "forecast":{"auto_refresh":s.forecast_plan_auto_refresh,"auto_apply":s.forecast_plan_auto_apply_enabled},
        "timed_export":{"enabled":ctx.export_config.schedule_enabled,"phase":export_phase(&ctx.export_state),"stop_pending":ctx.export_config.stop_pending}
    });
    value["calibration"] = json!({"supported":s.supports_battery_calibration,"stage":s.battery_calibration_stage,
        "phase":if s.supports_battery_calibration {calibration_phase(s.battery_calibration_stage)} else {"unavailable"}});
    value["maintenance"] = json!({"mode":s.battery_maintenance_mode,"phase":maintenance_phase(s.battery_maintenance_mode)});
    value["limits"] = json!({"reserve_soc":s.battery_reserve,"target_soc":s.target_soc,
        "charge_rate_raw":s.charge_rate,"discharge_rate_raw":s.discharge_rate,
        "battery_power_cutoff_percent":if s.device_type.uses_three_phase_schedule_slots() {Some(s.battery_power_cutoff)} else {None}});
    let mut conditions = Vec::new();
    let mut condition = |present: bool, code: &str, label: &str| {
        if present {
            conditions.push(json!({"code":code,"label":label}));
        }
    };
    // Faults precede restrictions; every simultaneous condition is retained.
    condition(s.inverter_trip, "inverter_trip", "Inverter trip");
    condition(
        s.battery_over_temp,
        "battery_over_temperature",
        "Battery over-temperature warning",
    );
    condition(
        !s.gateway_fault_codes.is_empty(),
        "gateway_fault",
        "Gateway fault reported",
    );
    condition(
        s.battery_modules
            .iter()
            .any(|b| b.bms_warnings.iter().any(|w| *w != 0)),
        "battery_warning",
        "Battery warning reported",
    );
    condition(
        s.grid_loss || !s.grid_online,
        "grid_offline",
        "Grid offline",
    );
    condition(
        s.temperature_limiter_active,
        "temperature_limiter",
        "Temperature protection active",
    );
    condition(
        s.load_limiter_active,
        "load_limiter",
        "Load protection active",
    );
    condition(charge_paused, "charge_pause", "Charging paused by schedule");
    condition(
        discharge_paused,
        "discharge_pause",
        "Discharging paused by schedule",
    );
    condition(
        matches!(mode, BatteryMode::EcoPaused),
        "eco_pause",
        "Eco Paused",
    );
    condition(
        matches!(mode, BatteryMode::ExportPaused),
        "export_pause",
        "Export Paused",
    );
    condition(
        s.supports_battery_calibration && (1..=6).contains(&s.battery_calibration_stage),
        "calibration",
        "Battery calibration active",
    );
    condition(
        s.supports_battery_calibration && s.battery_calibration_stage > 7,
        "unknown_calibration",
        "Unknown calibration stage",
    );
    condition(
        s.battery_pause_mode > 3,
        "unknown_pause_mode",
        "Unknown pause mode",
    );
    condition(
        s.battery_pause_mode != 0 && pause_window.is_none(),
        "pause_status_unknown",
        "Pause status unavailable",
    );
    let maintenance_label = format!(
        "Battery maintenance — {}",
        maintenance_phase(s.battery_maintenance_mode)
    );
    condition(
        s.battery_maintenance_mode != 0,
        "battery_maintenance",
        &maintenance_label,
    );
    condition(
        matches!(s.device_type, DeviceType::Unknown(_)),
        "unknown_device",
        "Unknown inverter model",
    );
    condition(
        matches!(s.device_type, DeviceType::PvInverter),
        "battery_unavailable",
        "PV-only inverter — battery unavailable",
    );
    condition(
        minute.is_none(),
        "inverter_clock_unavailable",
        "Inverter clock unavailable",
    );
    condition(
        s.adaptive_charge_enabled && s.adaptive_charge_state == "error",
        "adaptive_error",
        "Adaptive Charge error",
    );
    condition(
        matches!(ctx.export_state, TimedExportState::Error { .. }),
        "timed_export_error",
        "Timed Export error",
    );

    let (mut source, mut phase, mut title) = if !known_device {
        ("unknown", "unknown", "Unknown".to_string())
    } else if let Some(force) = &ctx.force {
        let expired = force.end_ms.is_some_and(|end| now_ms >= end);
        let readback_after_request = s.timestamp.saturating_mul(1000) > force.started_at_ms;
        let observed = match force.kind {
            ForceKind::Charge => {
                s.enable_charge
                    && s.battery_power_mode == 1
                    && charge_window == Some(true)
                    && !charge_paused
            }
            ForceKind::Discharge => {
                s.enable_discharge
                    && s.battery_power_mode == 0
                    && discharge_window == Some(true)
                    && !discharge_paused
            }
        };
        let phase = if expired {
            "expired"
        } else if observed && readback_after_request {
            "active"
        } else {
            "pending"
        };
        value["remaining_minutes"] =
            json!(force
                .end_ms
                .map(|end| end.saturating_sub(now_ms).max(0).saturating_add(59_999) / 60_000));
        value["quick_action"] = json!({"action":force.kind.code(),"phase":phase,
            "requested_at":DateTime::<Utc>::from_timestamp_millis(force.started_at_ms).map(|t| t.to_rfc3339()),
            "window_ends_at":force.end_ms.and_then(DateTime::<Utc>::from_timestamp_millis).map(|t| t.to_rfc3339())});
        (force.kind.code(), phase, force.kind.label().to_string())
    } else if ctx.export_state.owns_discharge_control()
        || matches!(
            ctx.export_state,
            TimedExportState::BlockedByPause | TimedExportState::Error { .. }
        )
    {
        (
            "timed_export",
            export_phase(&ctx.export_state),
            "Timed Export".into(),
        )
    } else if s.auto_winter_active {
        ("winter", "active", "Winter automation".into())
    } else if s.cosy_active || s.cosy_enabled {
        (
            "cosy",
            if s.cosy_active { "active" } else { "waiting" },
            "Cosy".into(),
        )
    } else if s.agile_enabled {
        ("agile", reported_phase(&s.agile_state), "Agile".into())
    } else if s.adaptive_charge_enabled {
        (
            "adaptive",
            reported_phase(&s.adaptive_charge_state),
            "Adaptive Charge".into(),
        )
    } else if charge == "active" {
        (
            "timed_charge",
            "active",
            format!("Timed Charge ({})", mode_label(mode)),
        )
    } else {
        ("inverter", "observed", mode_label(mode).into())
    };
    if s.temperature_limiter_active || s.load_limiter_active {
        source = "safety";
        phase = "restricted";
        // The limiters hold discharge; a Charge action is not itself
        // restricted, so do not claim it is.
        title = match ctx.force.as_ref().map(|f| f.kind) {
            Some(ForceKind::Charge) => format!("Safety limit active ({title})"),
            _ => format!("Discharge restricted ({title})"),
        };
    }
    let detail = match phase {
        "pending" => format!("awaiting inverter readback; battery {activity}"),
        "expired" => format!("window elapsed; battery {activity}"),
        "waiting" | "outside_window" => format!("waiting for next window; battery {activity}"),
        "observed" | "active" | "restricted" => activity.to_string(),
        other => format!("{}; battery {activity}", other.replace('_', " ")),
    };
    let mut summary = format!("{title} — {detail}");
    if let Some(minutes) = value["remaining_minutes"].as_i64().filter(|m| *m > 0) {
        summary.push_str(&format!("; {minutes} minutes remaining"));
    }
    for (name, state) in [
        ("timed charge", charge),
        ("timed export", export),
        ("timed demand", demand),
    ] {
        if state == "armed" {
            summary.push_str(&format!("; {name} armed"));
        }
    }
    if let Some(first) = conditions.first() {
        let label = first["label"].as_str().unwrap_or("Condition reported");
        if label != title {
            summary = format!("{label} — {summary}");
        }
    }
    value["summary"] = json!(summary);
    value["control_source"] = json!(source);
    value["control_phase"] = json!(phase);
    value["conditions"] = json!(conditions);
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{BatteryMode, BatteryState, DeviceType};

    const NOW: i64 = 1_800_000_000_000;

    fn snapshot() -> InverterSnapshot {
        InverterSnapshot {
            timestamp: NOW / 1000,
            inverter_time: "2027-01-15 12:00:00".into(),
            device_type: DeviceType::ACCoupled,
            battery_power_mode: 1,
            grid_online: true,
            battery_mode: BatteryMode::Eco,
            ..Default::default()
        }
    }

    fn status(s: &InverterSnapshot) -> Value {
        build_status(
            Some(s),
            ConnectionState::Connected,
            20,
            &Context::default(),
            NOW,
        )
    }

    fn slot(start: u8, end: u8) -> ScheduleSlot {
        ScheduleSlot {
            enabled: true,
            start_hour: start,
            end_hour: end,
            ..Default::default()
        }
    }

    #[test]
    fn summary_covers_every_battery_mode_and_activity() {
        for (mode, code, label) in [
            (BatteryMode::Eco, "eco", "Eco"),
            (BatteryMode::EcoPaused, "eco_paused", "Eco Paused"),
            (BatteryMode::TimedDemand, "timed_demand", "Timed Demand"),
            (BatteryMode::TimedExport, "timed_export", "Timed Export"),
            (BatteryMode::ExportPaused, "export_paused", "Export Paused"),
            (BatteryMode::Unknown, "unknown", "Unknown"),
        ] {
            for (activity, name) in [
                (BatteryState::Idle, "idle"),
                (BatteryState::Charging, "charging"),
                (BatteryState::Discharging, "discharging"),
            ] {
                let s = InverterSnapshot {
                    battery_mode: mode,
                    battery_state: activity,
                    ..snapshot()
                };
                let value = status(&s);
                assert_eq!(value["mode"], code);
                assert_eq!(value["activity"], name);
                assert!(value["summary"].as_str().unwrap().contains(label));
                assert_eq!(value["remaining_minutes"], Value::Null);
            }
        }
    }

    #[test]
    fn no_snapshot_stale_disconnected_and_future_readings_are_not_live() {
        for conn in [
            ConnectionState::Connected,
            ConnectionState::Disconnected,
            ConnectionState::Reconnecting,
        ] {
            let v = build_status(None, conn.clone(), 20, &Context::default(), NOW);
            assert_eq!(v["activity"], "unavailable");
            assert_eq!(v["observed_at"], Value::Null);
            assert_eq!(v["stale"], false, "never-observed is not stale");
            assert_eq!(v["age_seconds"], Value::Null);
            if conn != ConnectionState::Connected {
                let v = build_status(Some(&snapshot()), conn, 20, &Context::default(), NOW);
                assert_eq!(v["activity"], "unavailable");
                assert_eq!(v["mode"], "unknown");
            }
        }
        for seconds in [-61, 61] {
            let s = InverterSnapshot {
                timestamp: NOW / 1000 + seconds,
                ..snapshot()
            };
            let v = status(&s);
            assert_eq!(v["stale"], true);
            assert_eq!(v["conditions"], json!([]));
            assert_eq!(v["schedules"]["charge"], "unknown");
        }
        let s = InverterSnapshot {
            timestamp: NOW / 1000 - 60,
            ..snapshot()
        };
        assert_eq!(status(&s)["stale"], false);
        let s = InverterSnapshot {
            timestamp: NOW / 1000 - 90,
            ..snapshot()
        };
        assert_eq!(
            build_status(
                Some(&s),
                ConnectionState::Connected,
                30,
                &Context::default(),
                NOW
            )["stale"],
            false
        );
    }

    #[test]
    fn schedules_use_inverter_clock_and_activity_not_host_timezone() {
        let mut s = snapshot();
        s.enable_charge = true;
        s.charge_slots[0] = slot(11, 13);
        assert_eq!(status(&s)["schedules"]["charge"], "armed");
        s.battery_state = BatteryState::Charging;
        assert_eq!(status(&s)["schedules"]["charge"], "active");
        assert_ne!(status(&s)["control_source"], "force_charge");
        s.inverter_time = "2027-01-15 13:00:00".into();
        assert_eq!(status(&s)["schedules"]["charge"], "armed");
        s.charge_slots[0] = slot(23, 1);
        s.inverter_time = "2027-01-16 00:00:00".into();
        assert_eq!(status(&s)["schedules"]["charge"], "active");
        s.inverter_time.clear();
        assert_eq!(status(&s)["schedules"]["charge"], "unknown");
        s.charge_slots[0].start_hour = 99;
        s.inverter_time = "2027-01-15 12:00:00".into();
        assert_eq!(status(&s)["schedules"]["charge"], "unknown");
    }

    #[test]
    fn force_action_ownership_is_separate_from_observed_activity() {
        for kind in [ForceKind::Charge, ForceKind::Discharge] {
            let mut ctx = Context {
                force: Some(ForceWindow {
                    kind,
                    started_at_ms: NOW,
                    end_ms: Some(NOW + 3_600_000),
                }),
                ..Default::default()
            };
            let mut s = snapshot();
            let v = build_status(Some(&s), ConnectionState::Connected, 20, &ctx, NOW);
            assert_eq!(v["control_phase"], "pending");
            assert_eq!(v["remaining_minutes"], 60);
            s.enable_charge = kind == ForceKind::Charge;
            s.enable_discharge = kind == ForceKind::Discharge;
            s.battery_power_mode = if kind == ForceKind::Charge { 1 } else { 0 };
            s.charge_slots[0] = slot(11, 13);
            s.discharge_slots[0] = slot(11, 13);
            ctx.force.as_mut().unwrap().started_at_ms = NOW - 1000;
            let v = build_status(Some(&s), ConnectionState::Connected, 20, &ctx, NOW);
            assert_eq!(v["control_phase"], "active");
            assert_eq!(v["activity"], "idle");
            ctx.force.as_mut().unwrap().end_ms = Some(NOW);
            let v = build_status(Some(&s), ConnectionState::Connected, 20, &ctx, NOW);
            assert_eq!(v["control_phase"], "expired");
            assert_eq!(v["remaining_minutes"], 0);
            ctx.force.as_mut().unwrap().end_ms = None;
            assert_eq!(
                build_status(Some(&s), ConnectionState::Connected, 20, &ctx, NOW)
                    ["remaining_minutes"],
                Value::Null
            );
        }
    }

    #[test]
    fn raw_pause_window_is_inverted_for_timed_demand() {
        let mut s = snapshot();
        s.battery_pause_mode = 2;
        s.battery_pause_slot = slot(13, 11); // pause outside 11:00–13:00
        s.battery_state = BatteryState::Discharging;
        assert_eq!(status(&s)["schedules"]["demand_discharge"], "active");
        s.inverter_time = "2027-01-15 14:00:00".into();
        let v = status(&s);
        assert_eq!(v["schedules"]["demand_discharge"], "armed");
        assert!(v["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "discharge_pause"));
        s.battery_pause_mode = 1;
        assert!(status(&s)["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "charge_pause"));
        s.battery_pause_mode = 3;
        let v = status(&s);
        for code in ["charge_pause", "discharge_pause"] {
            assert!(v["conditions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["code"] == code));
        }
    }

    #[test]
    fn managed_export_stays_armed_when_physical_slots_are_cleared() {
        let ctx = Context {
            export_config: TimedExportConfig {
                schedule_enabled: true,
                slots: vec![slot(16, 19)],
                ..Default::default()
            },
            export_state: TimedExportState::Configured,
            ..Default::default()
        };
        let v = build_status(Some(&snapshot()), ConnectionState::Connected, 20, &ctx, NOW);
        assert_eq!(v["schedules"]["export"], "armed");
        assert_eq!(v["automation"]["timed_export"]["phase"], "configured");
    }

    #[test]
    fn automation_phases_and_overlapping_protections_are_preserved() {
        for phase in [
            "inactive",
            "baseline_pending",
            "outside_window",
            "preferred",
            "recovery",
            "suspended_auto_winter",
            "restoring",
            "error",
            "future_phase",
        ] {
            let s = InverterSnapshot {
                adaptive_charge_enabled: true,
                adaptive_charge_state: phase.into(),
                ..snapshot()
            };
            assert_eq!(status(&s)["automation"]["adaptive"]["phase"], phase);
        }
        for phase in ["idle", "charging", "discharging", "future_phase"] {
            let s = InverterSnapshot {
                agile_enabled: true,
                agile_state: phase.into(),
                ..snapshot()
            };
            assert_eq!(status(&s)["automation"]["agile"]["phase"], phase);
        }
        let s = InverterSnapshot {
            cosy_enabled: true,
            cosy_active: true,
            auto_winter_active: true,
            load_limiter_active: true,
            temperature_limiter_active: true,
            grid_loss: true,
            inverter_trip: true,
            battery_over_temp: true,
            ..snapshot()
        };
        let v = status(&s);
        for code in [
            "load_limiter",
            "temperature_limiter",
            "grid_offline",
            "inverter_trip",
            "battery_over_temperature",
        ] {
            assert!(
                v["conditions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["code"] == code),
                "{code}"
            );
        }
        assert_eq!(v["automation"]["cosy"]["active"], true);
        assert_eq!(v["automation"]["winter"]["active"], true);
        assert!(v["summary"].as_str().unwrap().starts_with("Inverter trip"));
    }

    #[test]
    fn all_managed_export_phases_are_reported_and_out_rank_waiting_automation() {
        for (state, phase) in [
            (TimedExportState::Off, "off"),
            (TimedExportState::Configured, "configured"),
            (
                TimedExportState::Entering {
                    polls_waiting: 0,
                    retries: 0,
                },
                "entering",
            ),
            (TimedExportState::Active, "active"),
            (
                TimedExportState::Exiting {
                    polls_waiting: 0,
                    retries: 0,
                },
                "exiting",
            ),
            (TimedExportState::BlockedByPause, "blocked_by_pause"),
            (
                TimedExportState::Error {
                    reason: "write failed".into(),
                },
                "error",
            ),
        ] {
            let owns = state.owns_discharge_control();
            let ctx = Context {
                export_state: state,
                ..Default::default()
            };
            let s = InverterSnapshot {
                agile_enabled: true,
                agile_state: "idle".into(),
                ..snapshot()
            };
            let v = build_status(Some(&s), ConnectionState::Connected, 20, &ctx, NOW);
            assert_eq!(v["automation"]["timed_export"]["phase"], phase);
            if owns {
                assert_eq!(v["control_source"], "timed_export");
            }
        }
    }

    #[test]
    fn scheduled_pauses_hold_force_actions_in_pending_until_released() {
        for (kind, pause_mode) in [(ForceKind::Charge, 1u8), (ForceKind::Discharge, 2)] {
            let ctx = Context {
                force: Some(ForceWindow {
                    kind,
                    started_at_ms: NOW - 1000,
                    end_ms: Some(NOW + 3_600_000),
                }),
                ..Default::default()
            };
            let mut s = snapshot();
            s.enable_charge = kind == ForceKind::Charge;
            s.enable_discharge = kind == ForceKind::Discharge;
            s.battery_power_mode = if kind == ForceKind::Charge { 1 } else { 0 };
            s.charge_slots[0] = slot(11, 13);
            s.discharge_slots[0] = slot(11, 13);
            // Pause window overlapping the action window: the inverter cannot
            // be performing the action, so the phase must stay pending.
            s.battery_pause_mode = pause_mode;
            s.battery_pause_slot = slot(11, 13);
            let v = build_status(Some(&s), ConnectionState::Connected, 20, &ctx, NOW);
            assert_eq!(v["control_phase"], "pending");
            let expected = if kind == ForceKind::Charge {
                "charge_pause"
            } else {
                "discharge_pause"
            };
            assert!(v["conditions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["code"] == expected));
            // With the pause released, the same readback confirms the action.
            s.battery_pause_mode = 0;
            assert_eq!(
                build_status(Some(&s), ConnectionState::Connected, 20, &ctx, NOW)["control_phase"],
                "active"
            );
        }
    }

    #[test]
    fn safety_limit_wording_matches_the_restricted_direction() {
        let mut s = InverterSnapshot {
            temperature_limiter_active: true,
            ..snapshot()
        };
        s.enable_charge = true;
        s.battery_power_mode = 1;
        s.charge_slots[0] = slot(11, 13);
        s.battery_state = BatteryState::Charging;
        let charge_ctx = Context {
            force: Some(ForceWindow {
                kind: ForceKind::Charge,
                started_at_ms: NOW - 1000,
                end_ms: Some(NOW + 3_600_000),
            }),
            ..Default::default()
        };
        let v = build_status(Some(&s), ConnectionState::Connected, 20, &charge_ctx, NOW);
        let summary = v["summary"].as_str().unwrap();
        // The active condition legitimately prefixes the title (summary rules),
        // so assert on containment rather than the exact start.
        assert!(
            summary.contains("Safety limit active (Force Charge)"),
            "{summary}"
        );
        assert!(!summary.contains("Discharge restricted"), "{summary}");

        let discharge_ctx = Context {
            force: Some(ForceWindow {
                kind: ForceKind::Discharge,
                started_at_ms: NOW - 1000,
                end_ms: Some(NOW + 3_600_000),
            }),
            ..Default::default()
        };
        s.battery_state = BatteryState::Discharging;
        s.enable_charge = false;
        s.enable_discharge = true;
        s.battery_power_mode = 0;
        s.discharge_slots[0] = slot(11, 13);
        let v = build_status(
            Some(&s),
            ConnectionState::Connected,
            20,
            &discharge_ctx,
            NOW,
        );
        let summary = v["summary"].as_str().unwrap();
        assert!(
            summary.contains("Discharge restricted (Force Discharge)"),
            "{summary}"
        );
    }

    #[test]
    fn safety_keeps_force_metadata_and_countdown_without_claiming_actual_export() {
        let ctx = Context {
            force: Some(ForceWindow {
                kind: ForceKind::Discharge,
                started_at_ms: NOW - 1000,
                end_ms: Some(NOW + 1),
            }),
            ..Default::default()
        };
        let s = InverterSnapshot {
            temperature_limiter_active: true,
            battery_state: BatteryState::Discharging,
            grid_power: 100,
            enable_discharge: true,
            battery_power_mode: 0,
            discharge_slots: std::array::from_fn(|_| slot(11, 13)),
            ..snapshot()
        };
        let v = build_status(Some(&s), ConnectionState::Connected, 20, &ctx, NOW);
        assert_eq!(v["quick_action"]["action"], "force_discharge");
        assert_eq!(v["control_source"], "safety");
        assert_eq!(v["remaining_minutes"], 1);
        assert!(!v["summary"].as_str().unwrap().contains("exporting"));
    }

    #[test]
    fn maintenance_warnings_and_pv_only_battery_unavailability_are_covered() {
        for (mode, phase) in [
            (0, "off"),
            (1, "discharging"),
            (2, "charging"),
            (3, "standby"),
            (255, "unknown"),
        ] {
            let s = InverterSnapshot {
                battery_maintenance_mode: mode,
                ..snapshot()
            };
            assert_eq!(status(&s)["maintenance"]["phase"], phase);
        }
        let mut s = snapshot();
        s.battery_modules
            .push(crate::inverter::model::BatteryModule {
                bms_warnings: vec![0, 1],
                ..Default::default()
            });
        assert!(status(&s)["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "battery_warning"));
        s.battery_modules[0].bms_warnings = vec![0, 0];
        assert!(!status(&s)["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "battery_warning"));
        s.device_type = DeviceType::PvInverter;
        assert_eq!(status(&s)["activity"], "unavailable");
        assert_eq!(status(&s)["schedules"]["export"], "unknown");
    }

    #[test]
    fn automation_control_source_precedence_follows_configuration() {
        // Cosy enabled-but-waiting outranks Agile and Adaptive in the chain.
        let s = InverterSnapshot {
            cosy_enabled: true,
            agile_enabled: true,
            adaptive_charge_enabled: true,
            agile_state: "idle".into(),
            ..snapshot()
        };
        let v = status(&s);
        assert_eq!(v["control_source"], "cosy");
        assert_eq!(v["control_phase"], "waiting");
        let s = InverterSnapshot {
            cosy_enabled: true,
            cosy_active: true,
            agile_enabled: true,
            agile_state: "charging".into(),
            ..snapshot()
        };
        let v = status(&s);
        assert_eq!(v["control_source"], "cosy");
        assert_eq!(v["control_phase"], "active");
        // Agile without Cosy; Adaptive only when Agile is off.
        let s = InverterSnapshot {
            agile_enabled: true,
            adaptive_charge_enabled: true,
            agile_state: "discharging".into(),
            ..snapshot()
        };
        let v = status(&s);
        assert_eq!(v["control_source"], "agile");
        assert_eq!(v["control_phase"], "discharging");
        let s = InverterSnapshot {
            adaptive_charge_enabled: true,
            adaptive_charge_state: "preferred".into(),
            ..snapshot()
        };
        let v = status(&s);
        assert_eq!(v["control_source"], "adaptive");
        assert_eq!(v["control_phase"], "preferred");
    }

    #[test]
    fn calibration_stages_unknown_devices_and_fault_details_are_explicit() {
        for (stage, phase) in [
            (0, "off"),
            (1, "discharging"),
            (2, "setting_lower_limit"),
            (3, "charging"),
            (4, "setting_upper_limit"),
            (5, "balancing"),
            (6, "setting_full_capacity"),
            (7, "finished"),
            (99, "unknown"),
        ] {
            let s = InverterSnapshot {
                supports_battery_calibration: true,
                battery_calibration_stage: stage,
                ..snapshot()
            };
            assert_eq!(status(&s)["calibration"]["phase"], phase);
        }
        let s = InverterSnapshot {
            device_type: DeviceType::Unknown(999),
            battery_pause_mode: 99,
            gateway_fault_codes: vec!["Fault 1".into()],
            ..snapshot()
        };
        let v = status(&s);
        assert_eq!(v["mode"], "unknown");
        assert!(v["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "unknown_device"));
        assert!(v["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "gateway_fault"));
    }
}
