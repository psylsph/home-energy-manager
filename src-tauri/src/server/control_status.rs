//! Cached battery operating summary for authenticated integrations.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inverter::model::{BatteryMode, BatteryState, DeviceType};

    const NOW: i64 = 1_800_000_000_000;

    fn snapshot() -> InverterSnapshot {
        InverterSnapshot { timestamp: NOW / 1000, inverter_time: "2027-01-15 12:00:00".into(),
            device_type: DeviceType::ACCoupled, battery_power_mode: 1, grid_online: true,
            battery_mode: BatteryMode::Eco, ..Default::default() }
    }

    fn status(s: &InverterSnapshot) -> Value {
        build_status(Some(s), ConnectionState::Connected, 20, &Context::default(), NOW)
    }

    fn slot(start: u8, end: u8) -> ScheduleSlot {
        ScheduleSlot { enabled: true, start_hour: start, end_hour: end, ..Default::default() }
    }

    #[test]
    fn summary_covers_every_battery_mode_and_activity() {
        for (mode, code, label) in [(BatteryMode::Eco,"eco","Eco"), (BatteryMode::EcoPaused,"eco_paused","Eco Paused"), (BatteryMode::TimedDemand,"timed_demand","Timed Demand"), (BatteryMode::TimedExport,"timed_export","Timed Export"), (BatteryMode::ExportPaused,"export_paused","Export Paused"), (BatteryMode::Unknown,"unknown","Unknown")] {
            for (activity, name) in [(BatteryState::Idle,"idle"),(BatteryState::Charging,"charging"),(BatteryState::Discharging,"discharging")] {
                let s = InverterSnapshot { battery_mode: mode, battery_state: activity, ..snapshot() };
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
        for conn in [ConnectionState::Connected,ConnectionState::Disconnected,ConnectionState::Reconnecting] {
            let v = build_status(None, conn.clone(),20,&Context::default(),NOW);
            assert_eq!(v["activity"], "unavailable");
            assert_eq!(v["observed_at"], Value::Null);
            if conn != ConnectionState::Connected {
                let v = build_status(Some(&snapshot()),conn,20,&Context::default(),NOW);
                assert_eq!(v["activity"], "unavailable");
                assert_eq!(v["mode"], "unknown");
            }
        }
        for seconds in [-61, 61] {
            let s = InverterSnapshot { timestamp: NOW/1000 + seconds, ..snapshot() };
            let v = status(&s);
            assert_eq!(v["stale"], true);
            assert_eq!(v["conditions"], json!([]));
            assert_eq!(v["schedules"]["charge"], "unknown");
        }
        let s = InverterSnapshot { timestamp: NOW/1000 - 60, ..snapshot() };
        assert_eq!(status(&s)["stale"], false);
        let s = InverterSnapshot { timestamp: NOW/1000 - 90, ..snapshot() };
        assert_eq!(build_status(Some(&s),ConnectionState::Connected,30,&Context::default(),NOW)["stale"],false);
    }

    #[test]
    fn schedules_use_inverter_clock_and_activity_not_host_timezone() {
        let mut s = snapshot();
        s.enable_charge = true;
        s.charge_slots[0] = slot(11,13);
        assert_eq!(status(&s)["schedules"]["charge"],"armed");
        s.battery_state = BatteryState::Charging;
        assert_eq!(status(&s)["schedules"]["charge"],"active");
        assert_ne!(status(&s)["control_source"],"force_charge");
        s.inverter_time = "2027-01-15 13:00:00".into();
        assert_eq!(status(&s)["schedules"]["charge"],"armed");
        s.charge_slots[0] = slot(23,1);
        s.inverter_time = "2027-01-16 00:00:00".into();
        assert_eq!(status(&s)["schedules"]["charge"],"active");
        s.inverter_time.clear();
        assert_eq!(status(&s)["schedules"]["charge"],"unknown");
        s.charge_slots[0].start_hour = 99;
        s.inverter_time = "2027-01-15 12:00:00".into();
        assert_eq!(status(&s)["schedules"]["charge"],"unknown");
    }

    #[test]
    fn force_action_ownership_is_separate_from_observed_activity() {
        for kind in [ForceKind::Charge, ForceKind::Discharge] {
            let mut ctx = Context { force: Some(ForceWindow { kind, started_at_ms: NOW, end_ms: Some(NOW+3_600_000) }), ..Default::default() };
            let mut s = snapshot();
            let v = build_status(Some(&s),ConnectionState::Connected,20,&ctx,NOW);
            assert_eq!(v["control_phase"],"pending");
            assert_eq!(v["remaining_minutes"],60);
            s.enable_charge = kind == ForceKind::Charge;
            s.enable_discharge = kind == ForceKind::Discharge;
            s.battery_power_mode = if kind == ForceKind::Charge {1} else {0};
            s.charge_slots[0] = slot(11,13);
            s.discharge_slots[0] = slot(11,13);
            ctx.force.as_mut().unwrap().started_at_ms = NOW - 1000;
            let v = build_status(Some(&s),ConnectionState::Connected,20,&ctx,NOW);
            assert_eq!(v["control_phase"],"active");
            assert_eq!(v["activity"],"idle");
            ctx.force.as_mut().unwrap().end_ms = Some(NOW);
            let v = build_status(Some(&s),ConnectionState::Connected,20,&ctx,NOW);
            assert_eq!(v["control_phase"],"expired");
            assert_eq!(v["remaining_minutes"],0);
            ctx.force.as_mut().unwrap().end_ms = None;
            assert_eq!(build_status(Some(&s),ConnectionState::Connected,20,&ctx,NOW)["remaining_minutes"],Value::Null);
        }
    }

    #[test]
    fn raw_pause_window_is_inverted_for_timed_demand() {
        let mut s = snapshot();
        s.battery_pause_mode = 2;
        s.battery_pause_slot = slot(13,11); // pause outside 11:00–13:00
        s.battery_state = BatteryState::Discharging;
        assert_eq!(status(&s)["schedules"]["demand_discharge"],"active");
        s.inverter_time = "2027-01-15 14:00:00".into();
        let v = status(&s);
        assert_eq!(v["schedules"]["demand_discharge"],"armed");
        assert!(v["conditions"].as_array().unwrap().iter().any(|c| c["code"]=="discharge_pause"));
        s.battery_pause_mode = 1;
        assert!(status(&s)["conditions"].as_array().unwrap().iter().any(|c| c["code"]=="charge_pause"));
        s.battery_pause_mode = 3;
        let v = status(&s);
        for code in ["charge_pause","discharge_pause"] { assert!(v["conditions"].as_array().unwrap().iter().any(|c| c["code"]==code)); }
    }

    #[test]
    fn managed_export_stays_armed_when_physical_slots_are_cleared() {
        let ctx = Context { export_config: TimedExportConfig { schedule_enabled:true, slots:vec![slot(16,19)], ..Default::default() }, export_state: TimedExportState::Configured, ..Default::default() };
        let v = build_status(Some(&snapshot()),ConnectionState::Connected,20,&ctx,NOW);
        assert_eq!(v["schedules"]["export"],"armed");
        assert_eq!(v["automation"]["timed_export"]["phase"],"configured");
    }

    #[test]
    fn automation_phases_and_overlapping_protections_are_preserved() {
        for phase in ["inactive","baseline_pending","outside_window","preferred","recovery","suspended_auto_winter","restoring","error","future_phase"] {
            let s = InverterSnapshot { adaptive_charge_enabled:true, adaptive_charge_state:phase.into(), ..snapshot() };
            assert_eq!(status(&s)["automation"]["adaptive"]["phase"],phase);
        }
        for phase in ["idle","charging","discharging","future_phase"] {
            let s = InverterSnapshot { agile_enabled:true, agile_state:phase.into(), ..snapshot() };
            assert_eq!(status(&s)["automation"]["agile"]["phase"],phase);
        }
        let s = InverterSnapshot { cosy_enabled:true, cosy_active:true, auto_winter_active:true,
            load_limiter_active:true, temperature_limiter_active:true, grid_loss:true,
            inverter_trip:true, battery_over_temp:true, ..snapshot() };
        let v = status(&s);
        for code in ["load_limiter","temperature_limiter","grid_offline","inverter_trip","battery_over_temperature"] {
            assert!(v["conditions"].as_array().unwrap().iter().any(|c| c["code"]==code),"{code}");
        }
        assert_eq!(v["automation"]["cosy"]["active"],true);
        assert_eq!(v["automation"]["winter"]["active"],true);
        assert!(v["summary"].as_str().unwrap().starts_with("Inverter trip"));
    }

    #[test]
    fn calibration_stages_unknown_devices_and_fault_details_are_explicit() {
        for (stage, phase) in [(0,"off"),(1,"discharging"),(2,"setting_lower_limit"),(3,"charging"),(4,"setting_upper_limit"),(5,"balancing"),(6,"setting_full_capacity"),(7,"finished"),(99,"unknown")] {
            let s = InverterSnapshot { supports_battery_calibration:true,battery_calibration_stage:stage,..snapshot() };
            assert_eq!(status(&s)["calibration"]["phase"],phase);
        }
        let s = InverterSnapshot { device_type:DeviceType::Unknown(999), battery_pause_mode:99,
            gateway_fault_codes:vec!["Fault 1".into()], ..snapshot() };
        let v = status(&s);
        assert_eq!(v["mode"],"unknown");
        assert!(v["conditions"].as_array().unwrap().iter().any(|c| c["code"]=="unknown_device"));
        assert!(v["conditions"].as_array().unwrap().iter().any(|c| c["code"]=="gateway_fault"));
    }
}
