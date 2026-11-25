//! A second, read-only inverter on the same supply (issue #456).
//!
//! Some houses have two GivEnergy units on one supply, and neither sees the
//! whole house: an All-in-One behind a Gateway next to an AC-coupled inverter
//! that carries the solar CT, for instance. HEM keeps controlling the first
//! unit (the *primary*) exactly as before. A second HEM, running on the other
//! inverter, serves that unit's snapshot; this module pulls it and folds the
//! house-level figures into the primary's snapshot.
//!
//! ## What is merged, and what is not
//!
//! Only the figures that describe the whole house are replaced: solar, grid
//! and home power, and the day totals. Everything the primary's own control
//! logic reads back (state of charge, battery power and capacity, schedules,
//! modes, pause registers) stays the primary's, so no automation ever
//! confuses the other unit's battery for the one it is steering. The other
//! unit's battery figures travel alongside in [`SecondUnitInfo`] for display.
//!
//! ## Invariants
//!
//! - Nothing is ever sent to the second HEM: this module only reads.
//! - A second unit that is unreachable, stale or not a different inverter
//!   leaves the primary's own figures untouched and says so in
//!   [`SecondUnitInfo::state`]; its last values are never frozen into the
//!   house figures.
//! - The merge is applied to the *published* snapshot only. The sanitizer's
//!   "previous reading" is the primary's own (`AppState::primary_snapshot`),
//!   so a merged total can never be fed back in and counted twice.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::inverter::model::{DeviceType, InverterSnapshot};
use crate::inverter::poll::AppState;

/// How old the second unit's data may be before it drops out of the merge.
/// Measured on this host's clock from when its reading last advanced (see
/// [`RemoteSource::accept`]), so a second HEM that answers but has stalled
/// drops out too, and a second HEM with a wrong clock doesn't.
pub const MAX_AGE_SECS: i64 = 120;

/// What the merge did with the second unit on this publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecondUnitState {
    /// Its figures are in the house totals.
    Merged,
    /// A second HEM is configured but hasn't answered yet.
    Unreachable,
    /// It answered before, but its data is now too old to use.
    Stale,
    /// It answered with something that can't be merged: the same inverter as
    /// the primary, a Gateway, or a HEM that itself merges a third unit.
    Invalid,
}

/// The second unit as the status views show it. Present on the primary's
/// snapshot whenever a second HEM is configured, so the page can say why the
/// house figures are, or are not, the whole house.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecondUnitInfo {
    pub state: SecondUnitState,
    /// Seconds since the second unit's reading was taken. `None` before the
    /// first answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_secs: Option<i64>,
    /// The second unit's own readings; only meaningful when `state` is
    /// [`SecondUnitState::Merged`].
    #[serde(default)]
    pub solar_power: i32,
    #[serde(default)]
    pub battery_power: i32,
    #[serde(default)]
    pub soc: u8,
    #[serde(default)]
    pub battery_capacity_kwh: f32,
    #[serde(default)]
    pub today_charge_kwh: f32,
    #[serde(default)]
    pub today_discharge_kwh: f32,
}

impl SecondUnitInfo {
    fn without_figures(state: SecondUnitState, age_secs: Option<i64>) -> Self {
        Self {
            state,
            age_secs,
            solar_power: 0,
            battery_power: 0,
            soc: 0,
            battery_capacity_kwh: 0.0,
            today_charge_kwh: 0.0,
            today_discharge_kwh: 0.0,
        }
    }
}

/// The most recent answer from the second HEM.
#[derive(Debug, Clone, Default)]
pub struct RemoteSource {
    /// Its snapshot, or `None` until the first answer. A later failed fetch
    /// leaves the last snapshot in place; its age decides when it drops out.
    pub snapshot: Option<InverterSnapshot>,
    /// Unix seconds, on this host's clock, when `snapshot`'s reading last
    /// advanced. A refetch of the same reading doesn't move it.
    pub received_at: i64,
}

impl RemoteSource {
    /// The state after a successful fetch at `now_secs`.
    ///
    /// The stamp moves only when the second HEM's reading differs from the
    /// last one, so a stalled second HEM that keeps serving its final
    /// snapshot ages out, and its own wall clock is never compared with ours.
    #[must_use]
    pub fn accept(&self, snapshot: InverterSnapshot, now_secs: i64) -> RemoteSource {
        let advanced = self
            .snapshot
            .as_ref()
            .is_none_or(|prev| prev.timestamp != snapshot.timestamp);
        RemoteSource {
            received_at: if advanced { now_secs } else { self.received_at },
            snapshot: Some(snapshot),
        }
    }
}

/// Check and tidy the second HEM's address as typed in Settings.
///
/// Empty means "not configured". Anything else must be `http://host[:port]`
/// or `https://host[:port]` and nothing more: no credentials, path, query or
/// fragment, because HEM only ever asks that address for `/api/snapshot`.
/// Returns the address without a trailing slash.
pub fn normalize_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .ok_or_else(|| "The second HEM address must start with http:// or https://".to_string())?;
    let authority = rest.trim_end_matches('/');
    if authority.is_empty() {
        return Err("The second HEM address needs a host, e.g. http://192.168.1.50:7338".into());
    }
    if authority.contains(['/', '?', '#', '@'])
        || authority
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(
            "The second HEM address is just the host and port, e.g. http://192.168.1.50:7338"
                .into(),
        );
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !authority.ends_with(']') => (h, Some(p)),
        _ => (authority, None),
    };
    if host.is_empty() || host == "[" {
        return Err("The second HEM address needs a host, e.g. http://192.168.1.50:7338".into());
    }
    // An IPv6 host must be bracketed, with the brackets balanced; otherwise
    // the last colon isn't a port separator and the address can never work.
    let bracketed = host.starts_with('[') && host.ends_with(']');
    if (host.contains(['[', ']']) && !bracketed) || (!bracketed && host.contains(':')) {
        return Err("Put an IPv6 address in square brackets, e.g. http://[fd00::5]:7338".into());
    }
    if let Some(p) = port {
        match p.parse::<u16>() {
            Ok(n) if n >= 1 => {}
            _ => return Err("The second HEM port must be between 1 and 65535".into()),
        }
    }
    let scheme_len = trimmed.len() - rest.len();
    Ok(format!("{}{}", &trimmed[..scheme_len], authority))
}

/// Fold the second unit into `primary`'s house-level figures.
///
/// `configured` is whether a second HEM address is set; when it isn't the
/// snapshot is left exactly as it was, with no [`SecondUnitInfo`].
pub fn apply(
    primary: &mut InverterSnapshot,
    configured: bool,
    remote: &RemoteSource,
    now_secs: i64,
) {
    if !configured {
        primary.second_unit = None;
        return;
    }
    let Some(second) = remote.snapshot.as_ref() else {
        primary.second_unit = Some(SecondUnitInfo::without_figures(
            SecondUnitState::Unreachable,
            None,
        ));
        return;
    };

    // Judged on this host's clock alone: `received_at` is when the second
    // HEM's reading last moved on, so the other machine's clock never counts.
    let age = now_secs.saturating_sub(remote.received_at).max(0);

    if is_same_or_unmergeable(primary, second) {
        primary.second_unit = Some(SecondUnitInfo::without_figures(
            SecondUnitState::Invalid,
            Some(age),
        ));
        return;
    }
    if age > MAX_AGE_SECS {
        primary.second_unit = Some(SecondUnitInfo::without_figures(
            SecondUnitState::Stale,
            Some(age),
        ));
        return;
    }

    // Grid and the import/export totals come from the second unit alone: both
    // units read the same incomer, so adding them would double the grid.
    // Solar adds, because each unit measures its own panels.
    let solar_power = primary
        .solar_power
        .saturating_add(second.solar_power)
        .max(0);
    let battery_total = primary.battery_power.saturating_add(second.battery_power);
    let grid_power = second.grid_power;
    // Battery power is + when discharging and grid is + when exporting, so
    // the house is what generation and both batteries supply, less export.
    let home_power = solar_power
        .saturating_add(battery_total)
        .saturating_sub(grid_power)
        .max(0);

    let today_solar = primary.today_solar_kwh + second.today_solar_kwh;
    let consumption = (today_solar
        + second.today_import_kwh
        + primary.today_discharge_kwh
        + second.today_discharge_kwh
        - second.today_export_kwh
        - primary.today_charge_kwh
        - second.today_charge_kwh)
        .max(0.0);

    primary.solar_power = solar_power;
    primary.grid_power = grid_power;
    primary.home_power = home_power;
    primary.today_solar_kwh = today_solar;
    primary.today_import_kwh = second.today_import_kwh;
    primary.today_export_kwh = second.today_export_kwh;
    primary.today_consumption_kwh = consumption;
    primary.home_energy_today_kwh = consumption;
    primary.second_unit = Some(SecondUnitInfo {
        state: SecondUnitState::Merged,
        age_secs: Some(age),
        solar_power: second.solar_power,
        battery_power: second.battery_power,
        soc: second.soc,
        battery_capacity_kwh: second.battery_capacity_kwh,
        today_charge_kwh: second.today_charge_kwh,
        today_discharge_kwh: second.today_discharge_kwh,
    });
}

/// True when merging `second` would double-count or loop: the same inverter
/// as the primary (the address points back at this HEM), a Gateway (its
/// figures are a different thing), or a HEM that is itself merging a unit.
fn is_same_or_unmergeable(primary: &InverterSnapshot, second: &InverterSnapshot) -> bool {
    let same_serial =
        !primary.inverter_serial.is_empty() && primary.inverter_serial == second.inverter_serial;
    same_serial || second.device_type == DeviceType::Gateway || second.second_unit.is_some()
}

/// How often the second HEM is asked for its snapshot.
const FETCH_INTERVAL: Duration = Duration::from_secs(10);

/// Give up on a fetch after this long, so a dead host never stalls the loop.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Read the snapshot out of a `GET /api/snapshot` reply.
fn parse_snapshot_reply(body: &str) -> Result<InverterSnapshot, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let why = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("no snapshot yet");
        return Err(why.to_string());
    }
    let data = value
        .get("data")
        .cloned()
        .ok_or_else(|| "reply has no data".to_string())?;
    serde_json::from_value(data).map_err(|e| format!("unreadable snapshot: {e}"))
}

/// Ask the second HEM for its snapshot. Blocking: run it off the async pool.
///
/// This is the only thing HEM ever sends to that address: one `GET` of
/// `/api/snapshot`. Redirects are refused so an address can't send HEM
/// somewhere else.
pub fn fetch_snapshot(base_url: &str) -> Result<InverterSnapshot, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .max_redirects(0)
        // Never route a LAN address through an environment proxy.
        .proxy(None)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .build();
    let mut response = ureq::Agent::new_with_config(agent)
        .get(format!("{base_url}/api/snapshot"))
        .call()
        .map_err(|e| format!("request failed: {e}"))?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("could not read the reply: {e}"))?;
    parse_snapshot_reply(&body)
}

/// Keep `state.second_unit` up to date for as long as the app runs.
///
/// Re-reads the address every cycle, so changing it in Settings takes effect
/// without a restart. With no address set it does nothing and holds nothing.
pub async fn run_fetch_loop(state: Arc<AppState>) {
    let mut last_ok: Option<bool> = None;
    let mut last_url = String::new();
    loop {
        let url = tokio::task::spawn_blocking(crate::settings::Settings::load)
            .await
            .map(|s| s.second_hem_url)
            .unwrap_or_default();
        if url != last_url {
            // A new address is a different unit: drop what the old one said.
            *state.second_unit.lock().unwrap_or_else(|p| p.into_inner()) = RemoteSource::default();
            last_ok = None;
            last_url = url.clone();
        }
        if !url.is_empty() {
            let target = url.clone();
            let result = tokio::task::spawn_blocking(move || fetch_snapshot(&target))
                .await
                .unwrap_or_else(|e| Err(format!("fetch task failed: {e}")));
            match result {
                Ok(snapshot) => {
                    let mut held = state.second_unit.lock().unwrap_or_else(|p| p.into_inner());
                    *held = held.accept(snapshot, chrono::Utc::now().timestamp());
                    drop(held);
                    if last_ok == Some(false) {
                        tracing::info!("Second HEM at {url} is answering again");
                    }
                    last_ok = Some(true);
                }
                Err(e) => {
                    // Warn once per outage; the page shows the state meanwhile.
                    if last_ok != Some(false) {
                        tracing::warn!("Second HEM at {url} didn't give a snapshot: {e}");
                    }
                    last_ok = Some(false);
                }
            }
        }
        tokio::time::sleep(FETCH_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    /// The AIO HEM's own reading on John's system (issue #456, 23 November
    /// snapshots, rounded): charging from the grid, no panels of its own.
    fn aio() -> InverterSnapshot {
        InverterSnapshot {
            timestamp: NOW,
            inverter_serial: "AIO0000001".into(),
            device_type: DeviceType::AllInOne6kW,
            solar_power: 0,
            battery_power: -400,
            grid_power: -400,
            home_power: 0,
            soc: 60,
            battery_capacity_kwh: 13.5,
            today_solar_kwh: 0.0,
            today_import_kwh: 35.5,
            today_export_kwh: 18.4,
            today_charge_kwh: 13.5,
            today_discharge_kwh: 2.1,
            today_consumption_kwh: 5.0,
            home_energy_today_kwh: 5.0,
            ..InverterSnapshot::default()
        }
    }

    /// The AC-coupled HEM's reading: the Enphase CT and two 9.4 kWh batteries.
    fn ac_coupled() -> InverterSnapshot {
        InverterSnapshot {
            timestamp: NOW,
            inverter_serial: "AC0000001".into(),
            device_type: DeviceType::ACCoupled,
            solar_power: 3000,
            battery_power: 1800,
            grid_power: 0,
            home_power: 4800,
            soc: 80,
            battery_capacity_kwh: 18.8,
            today_solar_kwh: 13.6,
            today_import_kwh: 35.5,
            today_export_kwh: 18.4,
            today_charge_kwh: 15.6,
            today_discharge_kwh: 10.6,
            today_consumption_kwh: 25.3,
            home_energy_today_kwh: 25.3,
            ..InverterSnapshot::default()
        }
    }

    fn fresh(second: InverterSnapshot) -> RemoteSource {
        RemoteSource {
            snapshot: Some(second),
            received_at: NOW,
        }
    }

    #[test]
    fn url_rejects_malformed_hosts_that_could_never_connect() {
        for bad in [
            "http://fe80::1",
            "http://[::1",
            "http://::1]:7338",
            "http://host\tname:7338",
            "http://host\u{7}:7338",
            "http://[fd00::5]x:7338",
        ] {
            assert!(normalize_url(bad).is_err(), "{bad:?} should be refused");
        }
        assert_eq!(
            normalize_url("http://[fd00::5]:7338/").as_deref(),
            Ok("http://[fd00::5]:7338")
        );
        assert_eq!(
            normalize_url("http://[fd00::5]").as_deref(),
            Ok("http://[fd00::5]")
        );
    }

    #[test]
    fn url_empty_is_not_configured() {
        assert_eq!(normalize_url("").unwrap(), "");
        assert_eq!(normalize_url("   ").unwrap(), "");
    }

    #[test]
    fn url_accepts_host_and_port_and_tidies() {
        assert_eq!(
            normalize_url(" http://192.168.1.50:7338/ ").unwrap(),
            "http://192.168.1.50:7338"
        );
        assert_eq!(normalize_url("http://hem-ac").unwrap(), "http://hem-ac");
        assert_eq!(
            normalize_url("https://hem.local:8443").unwrap(),
            "https://hem.local:8443"
        );
    }

    #[test]
    fn url_rejects_anything_but_scheme_host_port() {
        for bad in [
            "192.168.1.50:7338",
            "ftp://192.168.1.50",
            "http://",
            "http:///",
            "http://:7338",
            "http://host:0",
            "http://host:99999",
            "http://host:abc",
            "http://host/api/snapshot",
            "http://host?x=1",
            "http://host#frag",
            "http://user:pw@host",
            "http://ho st",
        ] {
            assert!(normalize_url(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn not_configured_leaves_the_snapshot_alone() {
        let mut p = aio();
        apply(&mut p, false, &fresh(ac_coupled()), NOW);
        assert_eq!(p.home_power, 0);
        assert_eq!(p.solar_power, 0);
        assert!(p.second_unit.is_none());
    }

    #[test]
    fn house_power_is_solar_plus_both_batteries_less_export() {
        let mut p = aio();
        apply(&mut p, true, &fresh(ac_coupled()), NOW);
        // 3000 solar + (-400 AIO charging + 1800 AC-coupled discharging) - 0.
        assert_eq!(p.solar_power, 3000);
        assert_eq!(p.grid_power, 0);
        assert_eq!(p.home_power, 4400);
        let info = p.second_unit.expect("info");
        assert_eq!(info.state, SecondUnitState::Merged);
        assert_eq!(info.battery_power, 1800);
        assert_eq!(info.soc, 80);
    }

    #[test]
    fn exporting_lowers_the_house_figure() {
        let mut second = ac_coupled();
        second.grid_power = 1000; // exporting
        let mut p = aio();
        apply(&mut p, true, &fresh(second), NOW);
        assert_eq!(p.grid_power, 1000);
        assert_eq!(p.home_power, 3400);
    }

    #[test]
    fn negative_house_clamps_to_zero() {
        let mut second = ac_coupled();
        second.solar_power = 0;
        second.battery_power = 0;
        second.grid_power = 0;
        let mut p = aio(); // AIO charging at 400 W
        apply(&mut p, true, &fresh(second), NOW);
        assert_eq!(p.home_power, 0);
    }

    #[test]
    fn day_totals_follow_the_merged_balance() {
        let mut p = aio();
        apply(&mut p, true, &fresh(ac_coupled()), NOW);
        // Import and export are counted once, from the second unit.
        assert!((p.today_import_kwh - 35.5).abs() < 1e-4);
        assert!((p.today_export_kwh - 18.4).abs() < 1e-4);
        assert!((p.today_solar_kwh - 13.6).abs() < 1e-4);
        // 13.6 solar + 35.5 import + (2.1 + 10.6) discharge - 18.4 export
        // - (13.5 + 15.6) charge = 14.3 kWh for the house (the day's check
        // on John's snapshots: about 14 kWh, where the AIO alone said 5).
        assert!((p.today_consumption_kwh - 14.3).abs() < 0.01);
        assert!((p.home_energy_today_kwh - 14.3).abs() < 0.01);
    }

    #[test]
    fn the_primarys_own_battery_and_control_fields_are_untouched() {
        let mut p = aio();
        apply(&mut p, true, &fresh(ac_coupled()), NOW);
        assert_eq!(p.soc, 60);
        assert_eq!(p.battery_power, -400);
        assert!((p.battery_capacity_kwh - 13.5).abs() < 1e-4);
        assert!((p.today_charge_kwh - 13.5).abs() < 1e-4);
        assert!((p.today_discharge_kwh - 2.1).abs() < 1e-4);
        assert_eq!(p.inverter_serial, "AIO0000001");
    }

    #[test]
    fn a_hybrid_with_its_own_panels_adds_them() {
        let mut p = aio();
        p.solar_power = 500;
        p.today_solar_kwh = 2.0;
        apply(&mut p, true, &fresh(ac_coupled()), NOW);
        assert_eq!(p.solar_power, 3500);
        assert!((p.today_solar_kwh - 15.6).abs() < 1e-4);
    }

    #[test]
    fn never_answered_is_unreachable_and_changes_nothing() {
        let mut p = aio();
        apply(&mut p, true, &RemoteSource::default(), NOW);
        assert_eq!(p.home_power, 0);
        let info = p.second_unit.expect("info");
        assert_eq!(info.state, SecondUnitState::Unreachable);
        assert_eq!(info.age_secs, None);
    }

    #[test]
    fn an_old_fetch_drops_out_instead_of_freezing() {
        let mut p = aio();
        let remote = RemoteSource {
            snapshot: Some(ac_coupled()),
            received_at: NOW - MAX_AGE_SECS - 1,
        };
        apply(&mut p, true, &remote, NOW);
        assert_eq!(p.home_power, 0, "AIO's own figure, not the frozen merge");
        assert_eq!(p.solar_power, 0);
        let info = p.second_unit.expect("info");
        assert_eq!(info.state, SecondUnitState::Stale);
        assert_eq!(info.age_secs, Some(MAX_AGE_SECS + 1));
    }

    #[test]
    fn a_second_hem_with_a_slow_clock_still_merges() {
        // Its wall clock is ten minutes behind this machine's. Freshness is
        // judged on this host's own clock, so that must not make it stale.
        let mut second = ac_coupled();
        second.timestamp = NOW - 600;
        let mut p = aio();
        apply(&mut p, true, &fresh(second), NOW);
        let info = p.second_unit.expect("info");
        assert_eq!(info.state, SecondUnitState::Merged);
        assert_eq!(info.age_secs, Some(0));
    }

    #[test]
    fn a_first_reply_is_stamped_with_this_hosts_time() {
        let r = RemoteSource::default().accept(ac_coupled(), NOW);
        assert_eq!(r.received_at, NOW);
        assert!(r.snapshot.is_some());
    }

    #[test]
    fn a_reading_that_moved_on_refreshes_the_stamp() {
        let prev = RemoteSource {
            snapshot: Some(ac_coupled()),
            received_at: NOW - 30,
        };
        let mut next = ac_coupled();
        next.timestamp += 10;
        assert_eq!(prev.accept(next, NOW).received_at, NOW);
    }

    #[test]
    fn a_clock_that_stepped_back_still_counts_as_a_new_reading() {
        let prev = RemoteSource {
            snapshot: Some(ac_coupled()),
            received_at: NOW - 30,
        };
        let mut next = ac_coupled();
        next.timestamp -= 3600;
        assert_eq!(prev.accept(next, NOW).received_at, NOW);
    }

    #[test]
    fn a_second_hem_that_keeps_serving_the_same_reading_goes_stale() {
        // Its API answers but its poll loop has stalled, so the reading never
        // advances. Even with a clock ten minutes ahead of ours it must drop out.
        let mut frozen = ac_coupled();
        frozen.timestamp = NOW + 600;
        let mut remote = RemoteSource::default().accept(frozen.clone(), NOW);
        remote = remote.accept(frozen.clone(), NOW + MAX_AGE_SECS + 1);
        let mut p = aio();
        apply(&mut p, true, &remote, NOW + MAX_AGE_SECS + 1);
        assert_eq!(p.second_unit.expect("info").state, SecondUnitState::Stale);
        assert_eq!(p.home_power, 0);
    }

    #[test]
    fn right_at_the_age_limit_still_merges() {
        let remote = RemoteSource {
            snapshot: Some(ac_coupled()),
            received_at: NOW - MAX_AGE_SECS,
        };
        let mut p = aio();
        apply(&mut p, true, &remote, NOW);
        assert_eq!(p.second_unit.expect("info").state, SecondUnitState::Merged);
    }

    #[test]
    fn the_same_inverter_is_invalid_not_double_counted() {
        // The address points back at this HEM.
        let mut second = ac_coupled();
        second.inverter_serial = "AIO0000001".into();
        let mut p = aio();
        apply(&mut p, true, &fresh(second), NOW);
        assert_eq!(p.second_unit.expect("info").state, SecondUnitState::Invalid);
        assert_eq!(p.solar_power, 0);
        assert_eq!(p.home_power, 0);
    }

    #[test]
    fn a_gateway_is_invalid() {
        let mut second = ac_coupled();
        second.device_type = DeviceType::Gateway;
        let mut p = aio();
        apply(&mut p, true, &fresh(second), NOW);
        assert_eq!(p.second_unit.expect("info").state, SecondUnitState::Invalid);
    }

    #[test]
    fn a_second_hem_that_merges_a_third_unit_is_invalid() {
        // Two HEMs pointing at each other would otherwise count each other's
        // merged totals again.
        let mut second = ac_coupled();
        second.second_unit = Some(SecondUnitInfo::without_figures(
            SecondUnitState::Merged,
            Some(1),
        ));
        let mut p = aio();
        apply(&mut p, true, &fresh(second), NOW);
        assert_eq!(p.second_unit.expect("info").state, SecondUnitState::Invalid);
    }

    #[test]
    fn a_missing_serial_on_the_primary_is_not_treated_as_the_same_unit() {
        let mut p = aio();
        p.inverter_serial.clear();
        let mut second = ac_coupled();
        second.inverter_serial.clear();
        apply(&mut p, true, &fresh(second), NOW);
        assert_eq!(p.second_unit.expect("info").state, SecondUnitState::Merged);
    }

    #[test]
    fn extreme_readings_saturate_instead_of_overflowing() {
        let mut second = ac_coupled();
        second.solar_power = i32::MAX;
        second.battery_power = i32::MAX;
        let mut p = aio();
        p.solar_power = i32::MAX;
        apply(&mut p, true, &fresh(second), NOW);
        assert!(p.home_power >= 0);
    }

    #[test]
    fn dropping_out_restores_the_primarys_figures_on_the_next_publish() {
        // `apply` runs on a fresh copy of the primary each publish, so a unit
        // that goes stale simply stops contributing. Prove it with two runs.
        let remote_ok = fresh(ac_coupled());
        let mut p1 = aio();
        apply(&mut p1, true, &remote_ok, NOW);
        assert_eq!(p1.home_power, 4400);

        let mut p2 = aio();
        apply(&mut p2, true, &remote_ok, NOW + MAX_AGE_SECS + 1);
        assert_eq!(p2.home_power, 0);
        assert_eq!(p2.second_unit.expect("info").state, SecondUnitState::Stale);
    }

    /// One-shot HTTP server answering every request with `body`.
    fn serve(status: &str, body: String) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let status = status.to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
            request.lines().map(str::to_string).collect()
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn fetch_reads_the_snapshot_and_sends_only_a_get_of_the_snapshot_path() {
        let body = serde_json::json!({ "ok": true, "data": ac_coupled() }).to_string();
        let (url, server) = serve("200 OK", body);
        let got = fetch_snapshot(&url).expect("snapshot");
        assert_eq!(got.inverter_serial, "AC0000001");
        assert_eq!(got.solar_power, 3000);
        let request = server.join().expect("server");
        assert_eq!(request[0], "GET /api/snapshot HTTP/1.1");
    }

    #[test]
    fn fetch_reports_a_second_hem_with_no_snapshot_yet() {
        let body =
            serde_json::json!({ "ok": false, "error": "No snapshot available yet" }).to_string();
        let (url, _server) = serve("200 OK", body);
        assert_eq!(
            fetch_snapshot(&url).unwrap_err(),
            "No snapshot available yet"
        );
    }

    #[test]
    fn fetch_rejects_a_reply_that_is_not_a_snapshot() {
        let (url, _s) = serve("200 OK", "<html>nope</html>".into());
        assert!(fetch_snapshot(&url).unwrap_err().contains("not JSON"));
        let (url, _s) = serve("200 OK", r#"{"ok":true}"#.into());
        assert!(fetch_snapshot(&url).is_err());
        let (url, _s) = serve("500 Internal Server Error", "boom".into());
        assert!(fetch_snapshot(&url).is_err());
    }

    #[test]
    fn fetch_refuses_to_follow_a_redirect() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });
        assert!(fetch_snapshot(&format!("http://{addr}")).is_err());
    }

    #[test]
    fn fetch_to_a_closed_port_errors_quickly() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        assert!(fetch_snapshot(&format!("http://127.0.0.1:{port}")).is_err());
    }
}
