//! HEM's own health, for external monitors (issue #503).
//!
//! `GET /api/health` (see [`crate::server::health`]) answers one overall
//! `healthy` / `degraded` / `unhealthy` with an entry per check. Everything
//! here is what HEM already knows: no inverter reads, no network calls and
//! no database queries, so a monitor can ask every minute.
//!
//! Only the core checks (the inverter, the poll loop and history) can make
//! the answer `unhealthy`; the rest only degrade it. A feature that isn't set
//! up is `disabled` and never counts. Each check's `detail` is a fixed code,
//! never free text, because the endpoint needs no key.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::inverter::poll::ConnectionState;

/// How long after start a missing first reading or poll beat reads as
/// `starting` rather than a failure.
const STARTUP_GRACE_S: i64 = 5 * 60;

/// Budget for a beat taken before work that can legitimately take a while:
/// a poll cycle (reads with retries, a queue of writes each retried while
/// the dongle is busy), a connect and its warm-up, or a LAN scan.
pub const POLL_BUSY_BUDGET: Duration = Duration::from_secs(15 * 60);

/// Budget for a beat taken inside one of the loop's once-a-second waits.
pub const POLL_IDLE_BUDGET: Duration = Duration::from_secs(2 * 60);

/// Free space under which the disk check warns. A backup short of room
/// reports itself through the backup check.
const DISK_WARN_BYTES: u64 = 500 * 1024 * 1024;

/// Free space under which the disk check fails.
const DISK_FAIL_BYTES: u64 = 100 * 1024 * 1024;

/// Weather is fetched every 15 minutes; four missed fetches is a problem.
const WEATHER_STALE_S: i64 = 60 * 60;

/// The solar forecast is fetched every 3 hours: two missed and an hour over.
const SOLAR_FORECAST_STALE_S: i64 = 7 * 60 * 60;

/// Octopus syncs every 6 hours: two missed and an hour over.
const OCTOPUS_STALE_S: i64 = 13 * 60 * 60;

// ---------------------------------------------------------------------------
// Marks the running parts of HEM leave for the health check
// ---------------------------------------------------------------------------

/// Process start and the poll loop's heartbeat, in [`crate::inverter::poll::AppState`].
#[derive(Debug)]
pub struct HealthMarks {
    started: Instant,
    started_wall: i64,
    /// Milliseconds after `started` by which the poll loop said it would beat
    /// again; 0 before its first beat. Monotonic, so clock steps can't trip it.
    poll_due_ms: AtomicU64,
    /// Wall-clock seconds of the poll loop's latest beat; 0 before the first.
    poll_beat_at: AtomicI64,
    /// Timestamp of the latest published reading; 0 before the first. Kept
    /// across reconnects (unlike `latest_snapshot`, which a disconnect
    /// clears), so an outage still shows how long ago HEM last had data.
    reading_at: AtomicI64,
    /// Per [`Feature`], when the health check first saw it enabled
    /// ([`UNOBSERVED`] before its first look, 0 while disabled).
    enabled_since: [AtomicI64; FEATURES],
}

/// The optional features whose first fetch gets a grace from being enabled.
#[derive(Debug, Clone, Copy)]
pub enum Feature {
    Backup = 0,
    Weather = 1,
    SolarForecast = 2,
    Octopus = 3,
}

const FEATURES: usize = 4;
const UNOBSERVED: i64 = -1;

impl Default for HealthMarks {
    fn default() -> Self {
        Self::new(Instant::now(), chrono::Utc::now().timestamp())
    }
}

/// What the poll loop's heartbeat shows at a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollBeat {
    /// The loop hasn't beaten yet.
    NotYet,
    /// The latest beat (wall seconds) and whether its budget has run out.
    Beat { at: i64, overdue: bool },
}

impl HealthMarks {
    pub fn new(started: Instant, started_wall: i64) -> Self {
        Self {
            started,
            started_wall,
            poll_due_ms: AtomicU64::new(0),
            poll_beat_at: AtomicI64::new(0),
            reading_at: AtomicI64::new(0),
            enabled_since: std::array::from_fn(|_| AtomicI64::new(UNOBSERVED)),
        }
    }

    /// A reading taken at `timestamp` was published.
    pub fn note_reading(&self, timestamp: i64) {
        self.reading_at.store(timestamp.max(1), Ordering::Relaxed);
    }

    /// The latest published reading's timestamp, if any.
    pub fn last_reading(&self) -> Option<i64> {
        Some(self.reading_at.load(Ordering::Relaxed)).filter(|&at| at > 0)
    }

    /// How long `feature` has been enabled at `now` (0 while disabled). A
    /// feature already on at the health check's first look counts from
    /// start; one switched on later, from the first look that saw it on.
    pub fn enabled_for_s(&self, feature: Feature, enabled: bool, now: i64) -> i64 {
        let slot = &self.enabled_since[feature as usize];
        if !enabled {
            slot.store(0, Ordering::Relaxed);
            return 0;
        }
        let since = match slot.load(Ordering::Relaxed) {
            UNOBSERVED => self.started_wall,
            0 => now,
            since => return (now - since).max(0),
        };
        slot.store(since, Ordering::Relaxed);
        (now - since).max(0)
    }

    /// Seconds since start at `now`.
    pub fn uptime_s(&self, now: Instant) -> i64 {
        i64::try_from(now.saturating_duration_since(self.started).as_secs()).unwrap_or(i64::MAX)
    }

    /// The poll loop is alive now and will beat again within `budget`.
    pub fn poll_beat(&self, budget: Duration) {
        self.poll_beat_at_time(Instant::now(), chrono::Utc::now().timestamp(), budget);
    }

    /// [`Self::poll_beat`] at injected times.
    pub fn poll_beat_at_time(&self, now: Instant, now_wall: i64, budget: Duration) {
        let due = now.saturating_duration_since(self.started) + budget;
        let due_ms = u64::try_from(due.as_millis()).unwrap_or(u64::MAX).max(1);
        self.poll_due_ms.store(due_ms, Ordering::Relaxed);
        self.poll_beat_at.store(now_wall, Ordering::Relaxed);
    }

    /// The heartbeat as seen at `now`.
    pub fn poll_state(&self, now: Instant) -> PollBeat {
        let due_ms = self.poll_due_ms.load(Ordering::Relaxed);
        if due_ms == 0 {
            return PollBeat::NotYet;
        }
        let elapsed_ms = u64::try_from(now.saturating_duration_since(self.started).as_millis())
            .unwrap_or(u64::MAX);
        PollBeat::Beat {
            at: self.poll_beat_at.load(Ordering::Relaxed),
            overdue: elapsed_ms > due_ms,
        }
    }
}

/// When history writes last succeeded and failed, and since when minute rows
/// have been waiting to be written. Kept on [`crate::history::HistoryDb`] as
/// atomics so the health check never takes the database lock.
///
/// Whether the latest write failed is a flag, not a comparison of the two
/// times, so a clock stepping back can't leave a recovered writer failing.
#[derive(Debug, Default)]
pub struct WriteMarks {
    ok_at: AtomicI64,
    err_at: AtomicI64,
    ready_since: AtomicI64,
    failing: AtomicBool,
}

/// A copy of [`WriteMarks`] at one moment; 0 means never / nothing waiting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryWrites {
    pub ok_at: i64,
    pub err_at: i64,
    pub ready_since: i64,
    /// The latest write failed.
    pub failing: bool,
}

impl WriteMarks {
    /// Rows became ready at `now`; keeps the earliest unwritten time.
    pub fn rows_ready(&self, now: i64) {
        let _ =
            self.ready_since
                .compare_exchange(0, now.max(1), Ordering::Relaxed, Ordering::Relaxed);
    }

    /// A write succeeded at `now`. Plain stores: a time stamped while the
    /// clock was ahead must not hide staleness after it is corrected.
    pub fn write_ok(&self, now: i64) {
        self.ok_at.store(now, Ordering::Relaxed);
        self.failing.store(false, Ordering::Relaxed);
    }

    /// A write failed at `now`.
    pub fn write_failed(&self, now: i64) {
        self.err_at.store(now, Ordering::Relaxed);
        self.failing.store(true, Ordering::Relaxed);
    }

    /// Every ready row has been written.
    pub fn all_written(&self) {
        self.ready_since.store(0, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> HistoryWrites {
        HistoryWrites {
            ok_at: self.ok_at.load(Ordering::Relaxed),
            err_at: self.err_at.load(Ordering::Relaxed),
            ready_since: self.ready_since.load(Ordering::Relaxed),
            failing: self.failing.load(Ordering::Relaxed),
        }
    }
}

/// When one notification channel last delivered and last failed (unix
/// seconds, 0 = never), and whether its latest send failed. Process-wide
/// statics, one per channel: the senders have no `AppState`.
#[derive(Debug, Default)]
pub struct DeliveryMarks {
    delivered_at: AtomicI64,
    failed_at: AtomicI64,
    failing: AtomicBool,
}

impl DeliveryMarks {
    pub const fn new() -> Self {
        Self {
            delivered_at: AtomicI64::new(0),
            failed_at: AtomicI64::new(0),
            failing: AtomicBool::new(false),
        }
    }

    /// Record one channel's send at `now`.
    pub fn record(&self, delivered: bool, now: i64) {
        let mark = if delivered {
            &self.delivered_at
        } else {
            &self.failed_at
        };
        mark.store(now, Ordering::Relaxed);
        self.failing.store(!delivered, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> NotifyMarks {
        NotifyMarks {
            delivered_at: self.delivered_at.load(Ordering::Relaxed),
            failed_at: self.failed_at.load(Ordering::Relaxed),
            failing: self.failing.load(Ordering::Relaxed),
        }
    }
}

/// A copy of [`DeliveryMarks`] at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotifyMarks {
    pub delivered_at: i64,
    pub failed_at: i64,
    /// The latest send failed.
    pub failing: bool,
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Starting,
    Disabled,
}

/// One check. `detail` is a fixed snake_case code, never free text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub status: CheckStatus,
    pub core: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<&'static str>,
    /// Unix seconds this part last worked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<i64>,
    /// Seconds since `last_ok`, never negative.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_s: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_mb: Option<u64>,
}

impl Check {
    fn new(core: bool, status: CheckStatus, detail: Option<&'static str>) -> Self {
        Self {
            status,
            core,
            detail,
            last_ok: None,
            age_s: None,
            free_mb: None,
        }
    }

    fn last_ok(mut self, now: i64, at: Option<i64>) -> Self {
        if let Some(at) = at.filter(|&at| at > 0) {
            self.last_ok = Some(at);
            self.age_s = Some((now - at).max(0));
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checks {
    pub inverter: Check,
    pub poll_loop: Check,
    pub history: Check,
    pub backup: Check,
    pub disk: Check,
    pub notifications: Check,
    pub weather: Check,
    pub solar_forecast: Check,
    pub octopus: Check,
}

impl Checks {
    fn all(&self) -> [&Check; 9] {
        [
            &self.inverter,
            &self.poll_loop,
            &self.history,
            &self.backup,
            &self.disk,
            &self.notifications,
            &self.weather,
            &self.solar_forecast,
            &self.octopus,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthReport {
    pub status: Overall,
    pub version: &'static str,
    pub uptime_s: i64,
    pub now: i64,
    pub checks: Checks,
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct BackupInput {
    pub enabled: bool,
    /// Seconds since backups were seen enabled.
    pub enabled_for_s: i64,
    pub interval_hours: u32,
    pub last_success_at: Option<i64>,
    /// Whether the latest attempt (scheduled or manual) worked, if any.
    pub last_attempt_ok: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DiskInput {
    /// `None` when free space couldn't be read.
    pub free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct NotifyInput {
    /// Alerts are on and at least one channel is set up.
    pub enabled: bool,
    /// One per configured channel.
    pub channels: Vec<NotifyMarks>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FetchInput {
    pub enabled: bool,
    /// Seconds since the feature was seen enabled.
    pub enabled_for_s: i64,
    pub last_ok: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OctopusInput {
    pub configured: bool,
    /// Seconds since it was seen configured.
    pub enabled_for_s: i64,
    pub last_sync_at: Option<i64>,
    /// The latest sync reported an error.
    pub failed: bool,
}

/// Everything the report is built from, copied out of the live state.
#[derive(Debug, Clone)]
pub struct HealthInputs {
    /// Unix seconds.
    pub now: i64,
    pub uptime_s: i64,
    pub poll_interval_secs: u64,
    pub host_configured: bool,
    pub connection: ConnectionState,
    /// The latest published reading's timestamp, kept across reconnects.
    pub last_reading: Option<i64>,
    pub poll: PollBeat,
    /// `None` when the history database isn't open.
    pub history: Option<HistoryWrites>,
    pub backup: BackupInput,
    pub disk: DiskInput,
    pub notifications: NotifyInput,
    pub weather: FetchInput,
    pub solar_forecast: FetchInput,
    pub octopus: OctopusInput,
}

// ---------------------------------------------------------------------------
// Judging
// ---------------------------------------------------------------------------

/// Judge every check. Served as 503 when [`Overall::Unhealthy`].
pub fn build_health(inputs: &HealthInputs) -> HealthReport {
    let checks = Checks {
        inverter: inverter_check(inputs),
        poll_loop: poll_check(inputs),
        history: history_check(inputs),
        backup: backup_check(inputs),
        disk: disk_check(inputs.disk),
        notifications: notify_check(inputs.now, &inputs.notifications),
        weather: fetch_check(inputs.now, inputs.weather, WEATHER_STALE_S),
        solar_forecast: fetch_check(inputs.now, inputs.solar_forecast, SOLAR_FORECAST_STALE_S),
        octopus: octopus_check(inputs),
    };
    let status = overall(&checks);
    HealthReport {
        status,
        version: env!("CARGO_PKG_VERSION"),
        uptime_s: inputs.uptime_s.max(0),
        now: inputs.now,
        checks,
    }
}

fn overall(checks: &Checks) -> Overall {
    let all = checks.all();
    if all.iter().any(|c| c.core && c.status == CheckStatus::Fail) {
        return Overall::Unhealthy;
    }
    if all.iter().any(|c| {
        matches!(
            c.status,
            CheckStatus::Warn | CheckStatus::Fail | CheckStatus::Starting
        )
    }) {
        return Overall::Degraded;
    }
    Overall::Healthy
}

fn starting(inputs: &HealthInputs) -> bool {
    inputs.uptime_s < STARTUP_GRACE_S
}

/// How old the latest reading may be: three polls, and never under 3 min.
fn reading_max_age_s(inputs: &HealthInputs) -> i64 {
    let interval = i64::try_from(inputs.poll_interval_secs).unwrap_or(i64::MAX / 4);
    interval.saturating_mul(3).max(180)
}

/// Judged by how old the latest reading is, whatever the connection state:
/// a dongle that drops and reconnects within three polls never fails, and
/// one that stays connected without new data does.
fn inverter_check(inputs: &HealthInputs) -> Check {
    use CheckStatus::{Fail, Ok, Starting};
    if !inputs.host_configured {
        return Check::new(true, Fail, Some("not_configured"));
    }
    let connection = match inputs.connection {
        ConnectionState::Connected => "connected",
        ConnectionState::Reconnecting => "reconnecting",
        ConnectionState::Disconnected => "disconnected",
    };
    let status = match inputs.last_reading {
        Some(at) if inputs.now - at <= reading_max_age_s(inputs) => Ok,
        None if starting(inputs) => Starting,
        _ => Fail,
    };
    let detail = match (status, inputs.last_reading, &inputs.connection) {
        (Fail, None, _) => "no_reading",
        (Fail, Some(_), ConnectionState::Connected) => "stale_reading",
        _ => connection,
    };
    Check::new(true, status, Some(detail)).last_ok(inputs.now, inputs.last_reading)
}

fn poll_check(inputs: &HealthInputs) -> Check {
    match inputs.poll {
        PollBeat::NotYet if starting(inputs) => Check::new(true, CheckStatus::Starting, None),
        PollBeat::NotYet => Check::new(true, CheckStatus::Fail, Some("not_running")),
        PollBeat::Beat { at, overdue } => {
            let check = if overdue {
                Check::new(true, CheckStatus::Fail, Some("stuck"))
            } else {
                Check::new(true, CheckStatus::Ok, None)
            };
            check.last_ok(inputs.now, Some(at))
        }
    }
}

/// How long a ready minute row may wait to be written.
fn history_max_wait_s(inputs: &HealthInputs) -> i64 {
    let interval = i64::try_from(inputs.poll_interval_secs).unwrap_or(i64::MAX / 4);
    interval.saturating_mul(2).saturating_add(120).max(300)
}

fn history_check(inputs: &HealthInputs) -> Check {
    let Some(writes) = inputs.history else {
        return Check::new(true, CheckStatus::Fail, Some("unavailable"));
    };
    let waiting_too_long =
        writes.ready_since > 0 && inputs.now - writes.ready_since > history_max_wait_s(inputs);
    let check = if writes.failing {
        Check::new(true, CheckStatus::Fail, Some("write_failed"))
    } else if waiting_too_long {
        Check::new(true, CheckStatus::Fail, Some("not_written"))
    } else if writes.ok_at == 0 {
        Check::new(true, CheckStatus::Ok, Some("idle"))
    } else {
        Check::new(true, CheckStatus::Ok, None)
    };
    check.last_ok(inputs.now, Some(writes.ok_at))
}

fn backup_check(inputs: &HealthInputs) -> Check {
    let b = inputs.backup;
    if !b.enabled {
        return Check::new(false, CheckStatus::Disabled, None);
    }
    let interval_s = i64::from(b.interval_hours.max(1)) * 3600;
    let check = if b.last_attempt_ok == Some(false) {
        Check::new(false, CheckStatus::Warn, Some("last_failed"))
    } else {
        match b.last_success_at {
            None if b.enabled_for_s <= interval_s + 3600 => {
                Check::new(false, CheckStatus::Ok, Some("not_yet"))
            }
            None => Check::new(false, CheckStatus::Warn, Some("never")),
            Some(at) if inputs.now - at > 2 * interval_s + 3600 => {
                Check::new(false, CheckStatus::Warn, Some("overdue"))
            }
            Some(_) => Check::new(false, CheckStatus::Ok, None),
        }
    };
    check.last_ok(inputs.now, b.last_success_at)
}

fn disk_check(disk: DiskInput) -> Check {
    let Some(free) = disk.free_bytes else {
        return Check::new(false, CheckStatus::Ok, Some("unavailable"));
    };
    let mut check = if free < DISK_FAIL_BYTES {
        Check::new(false, CheckStatus::Fail, Some("very_low_space"))
    } else if free < DISK_WARN_BYTES {
        Check::new(false, CheckStatus::Warn, Some("low_space"))
    } else {
        Check::new(false, CheckStatus::Ok, None)
    };
    check.free_mb = Some(free / (1024 * 1024));
    check
}

/// Warns when any configured channel's latest send failed, so one dead
/// channel shows even while another still gets through.
fn notify_check(now: i64, n: &NotifyInput) -> Check {
    if !n.enabled {
        return Check::new(false, CheckStatus::Disabled, None);
    }
    let delivered_at = n.channels.iter().map(|c| c.delivered_at).max().unwrap_or(0);
    let check = if n.channels.iter().any(|c| c.failing) {
        Check::new(false, CheckStatus::Warn, Some("last_failed"))
    } else if delivered_at == 0 {
        Check::new(false, CheckStatus::Ok, Some("none_sent"))
    } else {
        Check::new(false, CheckStatus::Ok, None)
    };
    check.last_ok(now, Some(delivered_at))
}

/// A fetch older than `stale_after_s` is stale; one that has never run is
/// fine until the feature has been on that long.
fn fetch_check(now: i64, fetch: FetchInput, stale_after_s: i64) -> Check {
    if !fetch.enabled {
        return Check::new(false, CheckStatus::Disabled, None);
    }
    let check = match fetch.last_ok {
        None if fetch.enabled_for_s <= stale_after_s => {
            Check::new(false, CheckStatus::Ok, Some("not_yet"))
        }
        None => Check::new(false, CheckStatus::Warn, Some("never")),
        Some(at) if now - at > stale_after_s => Check::new(false, CheckStatus::Warn, Some("stale")),
        Some(_) => Check::new(false, CheckStatus::Ok, None),
    };
    check.last_ok(now, fetch.last_ok)
}

fn octopus_check(inputs: &HealthInputs) -> Check {
    let o = inputs.octopus;
    if o.configured && o.failed {
        return Check::new(false, CheckStatus::Warn, Some("last_failed"))
            .last_ok(inputs.now, o.last_sync_at);
    }
    let sync = FetchInput {
        enabled: o.configured,
        enabled_for_s: o.enabled_for_s,
        last_ok: o.last_sync_at,
    };
    fetch_check(inputs.now, sync, OCTOPUS_STALE_S)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const NOW: i64 = 1_763_800_000;

    /// Everything set up and working, an hour after start.
    fn good() -> HealthInputs {
        HealthInputs {
            now: NOW,
            uptime_s: 3600,
            poll_interval_secs: 60,
            host_configured: true,
            connection: ConnectionState::Connected,
            last_reading: Some(NOW - 10),
            poll: PollBeat::Beat {
                at: NOW - 1,
                overdue: false,
            },
            history: Some(HistoryWrites {
                ok_at: NOW - 30,
                ..HistoryWrites::default()
            }),
            backup: BackupInput {
                enabled: true,
                enabled_for_s: 3600,
                interval_hours: 24,
                last_success_at: Some(NOW - 3600),
                last_attempt_ok: Some(true),
            },
            disk: DiskInput {
                free_bytes: Some(10 * 1024 * 1024 * 1024),
            },
            notifications: NotifyInput {
                enabled: true,
                channels: vec![NotifyMarks {
                    delivered_at: NOW - 600,
                    ..NotifyMarks::default()
                }],
            },
            weather: FetchInput {
                enabled: true,
                enabled_for_s: 3600,
                last_ok: Some(NOW - 300),
            },
            solar_forecast: FetchInput {
                enabled: true,
                enabled_for_s: 3600,
                last_ok: Some(NOW - 3600),
            },
            octopus: OctopusInput {
                configured: true,
                enabled_for_s: 3600,
                last_sync_at: Some(NOW - 3600),
                failed: false,
            },
        }
    }

    fn with(f: impl FnOnce(&mut HealthInputs)) -> HealthReport {
        let mut inputs = good();
        f(&mut inputs);
        build_health(&inputs)
    }

    #[test]
    fn everything_working_is_healthy() {
        let report = build_health(&good());
        assert_eq!(report.status, Overall::Healthy);
        for check in report.checks.all() {
            assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
        }
        assert_eq!(report.checks.inverter.age_s, Some(10));
        assert_eq!(report.checks.inverter.last_ok, Some(NOW - 10));
        assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(report.uptime_s, 3600);
    }

    #[test]
    fn nothing_set_up_is_disabled_and_still_healthy() {
        let report = with(|i| {
            i.backup.enabled = false;
            i.notifications.enabled = false;
            i.weather.enabled = false;
            i.solar_forecast.enabled = false;
            i.octopus.configured = false;
        });
        assert_eq!(report.status, Overall::Healthy);
        for check in [
            &report.checks.backup,
            &report.checks.notifications,
            &report.checks.weather,
            &report.checks.solar_forecast,
            &report.checks.octopus,
        ] {
            assert_eq!(check.status, CheckStatus::Disabled);
        }
    }

    #[test]
    fn a_disabled_feature_never_counts_however_stale_its_marks() {
        let report = with(|i| {
            i.octopus.configured = false;
            i.octopus.failed = true;
            i.weather.enabled = false;
            i.weather.last_ok = Some(NOW - 30 * 86_400);
            i.notifications.enabled = false;
            i.notifications.channels[0].failing = true;
            i.backup.enabled = false;
            i.backup.last_attempt_ok = Some(false);
        });
        assert_eq!(report.status, Overall::Healthy);
    }

    #[test]
    fn reading_age_threshold_is_three_polls_and_at_least_three_minutes() {
        // 60 s polls: 180 s is the limit.
        let at = |age, interval| {
            with(|i| {
                i.last_reading = Some(NOW - age);
                i.poll_interval_secs = interval;
            })
        };
        assert_eq!(at(180, 60).checks.inverter.status, CheckStatus::Ok);
        let stale = at(181, 60);
        assert_eq!(stale.checks.inverter.status, CheckStatus::Fail);
        assert_eq!(stale.checks.inverter.detail, Some("stale_reading"));
        assert_eq!(stale.status, Overall::Unhealthy);
        // 10 s polls still get three minutes.
        assert_eq!(at(180, 10).checks.inverter.status, CheckStatus::Ok);
        // 120 s polls get six.
        assert_eq!(at(360, 120).checks.inverter.status, CheckStatus::Ok);
        assert_eq!(at(361, 120).checks.inverter.status, CheckStatus::Fail);
    }

    #[test]
    fn a_long_outage_fails_the_inverter_but_not_history_and_keeps_its_age() {
        let report = with(|i| {
            i.connection = ConnectionState::Reconnecting;
            i.last_reading = Some(NOW - 3600);
            // History stopped when the readings did: nothing is waiting.
            i.history = Some(HistoryWrites {
                ok_at: NOW - 3600,
                ..HistoryWrites::default()
            });
        });
        assert_eq!(report.status, Overall::Unhealthy);
        assert_eq!(report.checks.inverter.status, CheckStatus::Fail);
        assert_eq!(report.checks.inverter.detail, Some("reconnecting"));
        assert_eq!(report.checks.inverter.last_ok, Some(NOW - 3600));
        assert_eq!(report.checks.inverter.age_s, Some(3600));
        assert_eq!(report.checks.history.status, CheckStatus::Ok);
        assert_eq!(report.checks.poll_loop.status, CheckStatus::Ok);
    }

    #[test]
    fn a_brief_reconnect_is_not_a_failure() {
        // Long after start, the dongle dropped 20 s after the last reading.
        for connection in [ConnectionState::Reconnecting, ConnectionState::Disconnected] {
            let report = with(|i| {
                i.connection = connection.clone();
                i.last_reading = Some(NOW - 30);
            });
            assert_eq!(report.status, Overall::Healthy, "{connection:?}");
            assert_eq!(report.checks.inverter.status, CheckStatus::Ok);
        }
        let back = with(|i| i.last_reading = Some(NOW - 170));
        assert_eq!(back.checks.inverter.status, CheckStatus::Ok);
        assert_eq!(back.checks.inverter.detail, Some("connected"));
        // Three polls without data fails, whatever the connection says.
        let gone = with(|i| {
            i.connection = ConnectionState::Disconnected;
            i.last_reading = Some(NOW - 181);
        });
        assert_eq!(gone.checks.inverter.detail, Some("disconnected"));
        assert_eq!(gone.status, Overall::Unhealthy);
    }

    #[test]
    fn no_host_is_not_configured() {
        let report = with(|i| {
            i.host_configured = false;
            i.connection = ConnectionState::Disconnected;
            i.last_reading = None;
        });
        assert_eq!(report.checks.inverter.status, CheckStatus::Fail);
        assert_eq!(report.checks.inverter.detail, Some("not_configured"));
    }

    #[test]
    fn a_restart_reads_as_starting_for_five_minutes_then_fails() {
        let at = |uptime| {
            with(|i| {
                i.uptime_s = uptime;
                i.connection = ConnectionState::Disconnected;
                i.last_reading = None;
                i.poll = PollBeat::NotYet;
            })
        };
        let early = at(STARTUP_GRACE_S - 1);
        assert_eq!(early.status, Overall::Degraded);
        assert_eq!(early.checks.inverter.status, CheckStatus::Starting);
        assert_eq!(early.checks.poll_loop.status, CheckStatus::Starting);
        assert_eq!(early.checks.inverter.detail, Some("disconnected"));
        let late = at(STARTUP_GRACE_S);
        assert_eq!(late.status, Overall::Unhealthy);
        assert_eq!(late.checks.inverter.status, CheckStatus::Fail);
        assert_eq!(late.checks.inverter.detail, Some("no_reading"));
        assert_eq!(late.checks.poll_loop.detail, Some("not_running"));
        // Connected with no reading yet is also only starting at first.
        let connected = with(|i| {
            i.uptime_s = 30;
            i.last_reading = None;
        });
        assert_eq!(connected.checks.inverter.status, CheckStatus::Starting);
        assert_eq!(connected.checks.inverter.detail, Some("connected"));
    }

    #[test]
    fn an_overdue_poll_beat_is_stuck() {
        let report = with(|i| {
            i.poll = PollBeat::Beat {
                at: NOW - 1000,
                overdue: true,
            }
        });
        assert_eq!(report.status, Overall::Unhealthy);
        assert_eq!(report.checks.poll_loop.detail, Some("stuck"));
        assert_eq!(report.checks.poll_loop.age_s, Some(1000));
    }

    #[test]
    fn history_fails_when_rows_wait_too_long_or_the_last_write_failed() {
        let waiting = |secs| {
            with(|i| {
                i.history = Some(HistoryWrites {
                    ok_at: NOW - 1000,
                    ready_since: NOW - secs,
                    ..HistoryWrites::default()
                })
            })
        };
        // 60 s polls: 300 s is the limit.
        assert_eq!(waiting(300).checks.history.status, CheckStatus::Ok);
        let late = waiting(301);
        assert_eq!(late.checks.history.status, CheckStatus::Fail);
        assert_eq!(late.checks.history.detail, Some("not_written"));
        assert_eq!(late.status, Overall::Unhealthy);

        let failed = with(|i| {
            i.history = Some(HistoryWrites {
                ok_at: NOW - 120,
                err_at: NOW - 60,
                ready_since: 0,
                failing: true,
            })
        });
        assert_eq!(failed.checks.history.detail, Some("write_failed"));
        assert_eq!(failed.checks.history.last_ok, Some(NOW - 120));

        // A later success clears it.
        let recovered = with(|i| {
            i.history = Some(HistoryWrites {
                ok_at: NOW - 30,
                err_at: NOW - 60,
                ready_since: 0,
                failing: false,
            })
        });
        assert_eq!(recovered.checks.history.status, CheckStatus::Ok);

        let missing = with(|i| i.history = None);
        assert_eq!(missing.checks.history.detail, Some("unavailable"));
        assert_eq!(missing.status, Overall::Unhealthy);

        let fresh = with(|i| i.history = Some(HistoryWrites::default()));
        assert_eq!(fresh.checks.history.status, CheckStatus::Ok);
        assert_eq!(fresh.checks.history.detail, Some("idle"));
        assert_eq!(fresh.checks.history.last_ok, None);
    }

    #[test]
    fn slow_polls_give_history_longer() {
        let report = with(|i| {
            i.poll_interval_secs = 300;
            i.history = Some(HistoryWrites {
                ok_at: NOW - 700,
                ready_since: NOW - 720,
                ..HistoryWrites::default()
            });
        });
        assert_eq!(report.checks.history.status, CheckStatus::Ok);
    }

    #[test]
    fn non_core_failures_only_degrade() {
        let report = with(|i| {
            i.backup.last_attempt_ok = Some(false);
            i.disk.free_bytes = Some(10 * 1024 * 1024);
            i.notifications.channels[0].failing = true;
            i.weather.last_ok = Some(NOW - 86_400);
            i.octopus.failed = true;
        });
        assert_eq!(report.status, Overall::Degraded);
        assert_eq!(report.checks.disk.status, CheckStatus::Fail);
        assert_eq!(report.checks.backup.detail, Some("last_failed"));
        assert_eq!(report.checks.notifications.detail, Some("last_failed"));
        assert_eq!(report.checks.weather.detail, Some("stale"));
        assert_eq!(report.checks.octopus.detail, Some("last_failed"));
    }

    #[test]
    fn backup_boundaries() {
        let at = |f: fn(&mut BackupInput)| {
            let mut i = good();
            f(&mut i.backup);
            build_health(&i).checks.backup
        };
        // Daily: overdue after 49 h.
        let ok = at(|b| b.last_success_at = Some(NOW - 49 * 3600));
        assert_eq!(ok.status, CheckStatus::Ok);
        let overdue = at(|b| b.last_success_at = Some(NOW - 49 * 3600 - 1));
        assert_eq!(overdue.detail, Some("overdue"));
        // Never: fine until backups have been on one interval and an hour.
        let mut i = good();
        i.backup.last_success_at = None;
        i.backup.last_attempt_ok = None;
        i.backup.enabled_for_s = 25 * 3600;
        i.uptime_s = 30 * 86_400;
        assert_eq!(build_health(&i).checks.backup.detail, Some("not_yet"));
        i.backup.enabled_for_s = 25 * 3600 + 1;
        assert_eq!(build_health(&i).checks.backup.detail, Some("never"));
        // A failed manual attempt counts as much as a scheduled one.
        let failed = at(|b| b.last_attempt_ok = Some(false));
        assert_eq!(failed.detail, Some("last_failed"));
    }

    #[test]
    fn disk_boundaries() {
        const MB: u64 = 1024 * 1024;
        let at = |free| {
            build_health(&HealthInputs {
                disk: DiskInput {
                    free_bytes: Some(free),
                },
                ..good()
            })
            .checks
            .disk
        };
        assert_eq!(at(500 * MB).status, CheckStatus::Ok);
        assert_eq!(at(500 * MB - 1).detail, Some("low_space"));
        assert_eq!(at(100 * MB).detail, Some("low_space"));
        assert_eq!(at(100 * MB - 1).detail, Some("very_low_space"));
        assert_eq!(at(2048 * MB).free_mb, Some(2048));
        let unknown = build_health(&HealthInputs {
            disk: DiskInput { free_bytes: None },
            ..good()
        });
        assert_eq!(unknown.checks.disk.detail, Some("unavailable"));
        assert_eq!(unknown.status, Overall::Healthy);
    }

    #[test]
    fn notifications_recover_on_a_later_delivery() {
        let failed = with(|i| i.notifications.channels[0].failing = true);
        assert_eq!(failed.checks.notifications.status, CheckStatus::Warn);
        assert_eq!(failed.checks.notifications.last_ok, Some(NOW - 600));
        let marks = DeliveryMarks::new();
        marks.record(false, NOW - 300);
        assert!(marks.snapshot().failing);
        marks.record(true, NOW - 100);
        let recovered = with(|i| i.notifications.channels = vec![marks.snapshot()]);
        assert_eq!(recovered.checks.notifications.status, CheckStatus::Ok);
        let none = with(|i| i.notifications.channels[0].delivered_at = 0);
        assert_eq!(none.checks.notifications.detail, Some("none_sent"));
        assert_eq!(none.status, Overall::Healthy);
    }

    #[test]
    fn one_dead_channel_shows_whichever_order_they_send_in() {
        let working = NotifyMarks {
            delivered_at: NOW - 60,
            ..NotifyMarks::default()
        };
        let dead = NotifyMarks {
            failed_at: NOW - 60,
            failing: true,
            ..NotifyMarks::default()
        };
        for channels in [vec![working, dead], vec![dead, working]] {
            let report = with(|i| i.notifications.channels = channels);
            assert_eq!(report.checks.notifications.detail, Some("last_failed"));
            assert_eq!(report.checks.notifications.last_ok, Some(NOW - 60));
        }
    }

    #[test]
    fn fetch_boundaries() {
        let weather = |age| with(|i| i.weather.last_ok = Some(NOW - age)).checks.weather;
        assert_eq!(weather(WEATHER_STALE_S).status, CheckStatus::Ok);
        assert_eq!(weather(WEATHER_STALE_S + 1).detail, Some("stale"));
        let forecast = |age| {
            with(|i| i.solar_forecast.last_ok = Some(NOW - age))
                .checks
                .solar_forecast
        };
        assert_eq!(forecast(SOLAR_FORECAST_STALE_S).status, CheckStatus::Ok);
        assert_eq!(forecast(SOLAR_FORECAST_STALE_S + 1).detail, Some("stale"));
        let octopus = |age| {
            with(|i| i.octopus.last_sync_at = Some(NOW - age))
                .checks
                .octopus
        };
        assert_eq!(octopus(OCTOPUS_STALE_S).status, CheckStatus::Ok);
        assert_eq!(octopus(OCTOPUS_STALE_S + 1).detail, Some("stale"));
        // Never fetched: fine until the feature has been on as long as a
        // fetch takes to go stale, however long HEM has been running.
        let never = |enabled_for_s| {
            with(|i| {
                i.uptime_s = 30 * 86_400;
                i.solar_forecast.enabled_for_s = enabled_for_s;
                i.solar_forecast.last_ok = None;
            })
            .checks
            .solar_forecast
        };
        assert_eq!(never(SOLAR_FORECAST_STALE_S).detail, Some("not_yet"));
        assert_eq!(never(SOLAR_FORECAST_STALE_S + 1).detail, Some("never"));
    }

    #[test]
    fn a_backwards_clock_never_gives_a_negative_age() {
        let report = with(|i| {
            i.last_reading = Some(NOW + 120);
            i.weather.last_ok = Some(NOW + 600);
        });
        assert_eq!(report.checks.inverter.age_s, Some(0));
        assert_eq!(report.checks.inverter.status, CheckStatus::Ok);
        assert_eq!(report.checks.weather.age_s, Some(0));
    }

    #[test]
    fn the_report_carries_only_the_pinned_keys() {
        let mut i = good();
        i.disk.free_bytes = Some(1);
        let v = serde_json::to_value(build_health(&i)).unwrap();
        let keys = |v: &Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(keys(&v), ["checks", "now", "status", "uptime_s", "version"]);
        assert_eq!(
            keys(&v["checks"]),
            [
                "backup",
                "disk",
                "history",
                "inverter",
                "notifications",
                "octopus",
                "poll_loop",
                "solar_forecast",
                "weather"
            ]
        );
        let allowed = ["age_s", "core", "detail", "free_mb", "last_ok", "status"];
        for (name, check) in v["checks"].as_object().unwrap() {
            for key in keys(check) {
                assert!(allowed.contains(&key.as_str()), "{name}.{key}");
            }
            if let Some(detail) = check.get("detail") {
                let d = detail.as_str().unwrap();
                assert!(
                    d.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                    "{name}.detail must be a code, got {d:?}"
                );
            }
        }
        assert_eq!(v["status"], "degraded");
        assert_eq!(v["checks"]["disk"]["status"], "fail");
        assert_eq!(v["checks"]["inverter"]["core"], true);
    }

    #[test]
    fn poll_marks_go_overdue_only_after_their_budget() {
        let start = Instant::now();
        let marks = HealthMarks::new(start, NOW);
        assert_eq!(marks.poll_state(start), PollBeat::NotYet);
        marks.poll_beat_at_time(start, NOW, Duration::from_secs(120));
        let at = |s| marks.poll_state(start + Duration::from_secs(s));
        assert_eq!(
            at(120),
            PollBeat::Beat {
                at: NOW,
                overdue: false
            }
        );
        assert_eq!(
            at(121),
            PollBeat::Beat {
                at: NOW,
                overdue: true
            }
        );
        // A fresh beat resets the budget.
        marks.poll_beat_at_time(
            start + Duration::from_secs(200),
            NOW + 200,
            POLL_IDLE_BUDGET,
        );
        assert_eq!(
            at(300),
            PollBeat::Beat {
                at: NOW + 200,
                overdue: false
            }
        );
        assert_eq!(marks.uptime_s(start + Duration::from_secs(300)), 300);
        // An instant before start (can't happen, but) is never negative.
        assert_eq!(marks.uptime_s(start), 0);
    }

    #[test]
    fn the_last_reading_survives_until_a_newer_one() {
        let marks = HealthMarks::new(Instant::now(), NOW);
        assert_eq!(marks.last_reading(), None);
        marks.note_reading(NOW - 5);
        assert_eq!(marks.last_reading(), Some(NOW - 5));
        marks.note_reading(NOW);
        assert_eq!(marks.last_reading(), Some(NOW));
    }

    #[test]
    fn a_feature_counts_from_start_or_from_when_it_was_switched_on() {
        let marks = HealthMarks::new(Instant::now(), NOW - 3600);
        // On at the first look: counted from start.
        assert_eq!(marks.enabled_for_s(Feature::Weather, true, NOW), 3600);
        assert_eq!(marks.enabled_for_s(Feature::Weather, true, NOW + 60), 3660);
        // Off at the first look, switched on later: counted from then.
        assert_eq!(marks.enabled_for_s(Feature::Octopus, false, NOW), 0);
        assert_eq!(marks.enabled_for_s(Feature::Octopus, true, NOW + 100), 0);
        assert_eq!(marks.enabled_for_s(Feature::Octopus, true, NOW + 400), 300);
        // Off and on again restarts it.
        assert_eq!(marks.enabled_for_s(Feature::Weather, false, NOW + 500), 0);
        assert_eq!(marks.enabled_for_s(Feature::Weather, true, NOW + 600), 0);
        // A backwards clock never gives a negative time.
        assert_eq!(marks.enabled_for_s(Feature::Weather, true, NOW), 0);
    }

    #[test]
    fn write_marks_keep_the_earliest_wait_and_clear_when_written() {
        let marks = WriteMarks::default();
        marks.rows_ready(100);
        marks.rows_ready(160);
        assert_eq!(marks.snapshot().ready_since, 100);
        marks.write_ok(170);
        marks.all_written();
        assert_eq!(
            marks.snapshot(),
            HistoryWrites {
                ok_at: 170,
                ..HistoryWrites::default()
            }
        );
        marks.rows_ready(200);
        marks.write_failed(205);
        assert_eq!(marks.snapshot().ready_since, 200);
        assert_eq!(marks.snapshot().err_at, 205);
        assert!(marks.snapshot().failing);
        // A success after the clock steps back still clears the failure, and
        // its own (earlier) time is what's shown.
        marks.write_ok(150);
        assert!(!marks.snapshot().failing);
        assert_eq!(marks.snapshot().ok_at, 150);
    }

    #[test]
    fn delivery_marks_keep_the_latest_of_each() {
        let marks = DeliveryMarks::new();
        marks.record(true, 100);
        marks.record(false, 120);
        assert!(marks.snapshot().failing);
        // A delivery after the clock stepped back still clears the failure.
        marks.record(true, 90);
        assert_eq!(
            marks.snapshot(),
            NotifyMarks {
                delivered_at: 90,
                failed_at: 120,
                failing: false
            }
        );
    }
}
